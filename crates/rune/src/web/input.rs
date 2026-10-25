use super::*;
use wasm_bindgen::{JsCast, JsValue, prelude::*};
use winit::event::{ElementState, MouseButton, TouchPhase};
use winit::keyboard::ModifiersState;

#[wasm_bindgen(module = "/src/web/bridge.js")]
extern "C" {
    fn mount(canvas: &web_sys::HtmlCanvasElement, callback: &js_sys::Function) -> JsValue;
    fn sync(handle: &JsValue, key: &str, text: &str, base: usize, anchor: u32, focus: u32, secure: bool);
    fn activate(handle: &JsValue);
    fn destroy(handle: &JsValue);
}

pub(super) struct Bridge {
    handle: JsValue,
    _callback: Closure<dyn FnMut(String)>,
    dispose_viewport: js_sys::Function,
    mods: ModifiersState,
    touch: Option<Touch>,
    mirror: crate::runtime::web_runtime::Mirror,
}
struct Touch {
    id: u64,
    scroll: Option<crate::NodeId>,
    origin: (f32, f32),
    last: (f32, f32),
    at: Instant,
    velocity: f32,
    pan: bool,
    /// The pan moves a horizontal viewport (picked when it starts).
    horizontal: bool,
}

impl Bridge {
    pub(super) fn new<S: 'static>(canvas: &web_sys::HtmlCanvasElement,
        running: std::rc::Weak<RefCell<Option<Running<S>>>>, window: Arc<Window>) -> Self {
        let callback = Closure::new(move |event: String| {
            let Some(running) = running.upgrade() else { return };
            let event = zeroize::Zeroizing::new(event);
            if let Ok(mut event) = serde_json::from_str(&event) {
                if let Some(r) = &mut *running.borrow_mut() { r.input_event(&mut event); }
                window.request_redraw();
            }
        });
        let dispose_viewport = platform::viewport(canvas, callback.as_ref().unchecked_ref());
        Self { handle: mount(canvas, callback.as_ref().unchecked_ref()), _callback: callback, dispose_viewport,
            mods: ModifiersState::empty(), touch: None, mirror: Default::default() }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.dispose_viewport.call0(&JsValue::NULL);
        destroy(&self.handle);
    }
}

impl<S: 'static> Running<S> {
    pub(super) fn sync_input(&mut self, focus: bool) {
        if self.rt.web_context(&mut self.input.mirror) {
            let m = &self.input.mirror;
            sync(&self.input.handle, &m.key, &m.text, m.base, m.anchor, m.focus, m.secure);
        }
        if focus { activate(&self.input.handle); }
    }

    pub(super) fn input_event(&mut self, event: &mut serde_json::Value) {
        let mut activate = false;
        match event["kind"].as_str() {
            Some("viewport") => {
                let insets = std::array::from_fn(|i| event["insets"][i].as_f64().unwrap_or(0.0).max(0.0) as f32);
                if INSETS.replace(insets) != insets { self.rt.invalidate(); }
            }
            Some("motion") => self.rt.set_system_reduced_motion(event["reduced"].as_bool().unwrap_or(false)),
            Some("focus") => self.rt.window_focus(event["focused"].as_bool().unwrap_or(false)),
            Some("visibility") => {
                self.shared.visible.store(event["visible"].as_bool().unwrap_or(false), Ordering::Release);
                return;
            }
            Some("edit" | "selection" | "preedit") => self.rt.web_edit(event),
            Some("sync") => self.input.mirror.key.clear(), // Composition's final sync was deferred by the DOM.
            Some("paste") => {
                let content = crate::ClipboardContent::new(event["text"].as_str().unwrap_or_default());
                self.rt.paste_content(match event["html"].as_str().filter(|html| !html.is_empty()) {
                    Some(html) => content.with_html(html),
                    None => content,
                });
            }
            Some("secure") => {
                if let serde_json::Value::String(text) = event["text"].take() {
                    self.rt.web_secure(event["key"].as_str().unwrap_or_default(), text);
                }
            }
            Some("key") => {
                let flag = |key: &str| event[key].as_bool().unwrap_or(false);
                let mut mods = ModifiersState::empty();
                mods.set(ModifiersState::SHIFT, flag("shift"));
                mods.set(ModifiersState::CONTROL, flag("ctrl"));
                mods.set(ModifiersState::ALT, flag("alt"));
                mods.set(ModifiersState::SUPER, flag("meta"));
                let key = match event["key"].as_str().unwrap_or_default() {
                    "Enter" => crate::Key::Enter, "Tab" => crate::Key::Tab, "Escape" => crate::Key::Escape,
                    "ArrowLeft" => crate::Key::Left, "ArrowRight" => crate::Key::Right,
                    "ArrowUp" => crate::Key::Up, "ArrowDown" => crate::Key::Down,
                    "Home" => crate::Key::Home, "End" => crate::Key::End,
                    "PageUp" => crate::Key::PageUp, "PageDown" => crate::Key::PageDown,
                    "Backspace" => crate::Key::Backspace, "Delete" => crate::Key::Delete,
                    name if name.chars().count() == 1 && (mods.control_key() || mods.super_key()) => crate::Key::Command(name.chars().next().unwrap().to_ascii_lowercase()),
                    _ => return,
                };
                let event_key = crate::KeyEvent { key, mods: modifiers(mods) };
                activate = event_key.key == crate::Key::Tab;
                if flag("repeat") { self.rt.repeat_key(event_key); } else { self.rt.key(event_key); }
            }
            _ => {}
        }
        // Commit within the DOM event: clipboard, link and soft-keyboard APIs need activation.
        self.rt.frame(0.0);
        self.clipboard.flush(&mut self.rt);
        self.sync_input(activate);
    }

    pub(super) fn window_input(&mut self, event: &WindowEvent, window: &Window) -> bool {
        let scale = window.scale_factor() as f32;
        let mut activate = false;
        let mut immediate = false;
        match event {
            WindowEvent::ModifiersChanged(mods) => { self.input.mods = mods.state(); return false; }
            WindowEvent::CursorMoved { position, .. } if self.input.touch.is_none() => {
                self.rt.pointer_move(position.x as f32 / scale, position.y as f32 / scale);
            }
            WindowEvent::CursorLeft { .. } if self.input.touch.is_none() => self.rt.pointer_leave(),
            WindowEvent::MouseInput { button, state, .. } if self.input.touch.is_none() => {
                immediate = true;
                let Some((x, y)) = self.rt.pointer() else { return false };
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => { self.rt.pointer_down(x, y); activate = true; }
                    (MouseButton::Left, ElementState::Released) => self.rt.pointer_up(x, y),
                    (MouseButton::Right, ElementState::Pressed) => self.rt.secondary_down(x, y),
                    (MouseButton::Right, ElementState::Released) => self.rt.secondary_up(x, y),
                    _ => return false,
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Trackpad pinch is ctrl+wheel. Rune ignores it; the page never zooms.
                if self.input.mods.control_key() { return false; }
                let Some((x, y)) = self.rt.pointer() else { return false };
                let (dx, dy) = crate::runtime::wheel_delta(*delta, scale, self.input.mods.shift_key());
                self.rt.wheel_xy(x, y, dx, dy);
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                immediate = true;
                let mut logical = (event.state == ElementState::Pressed).then(|| crate::window::map_key(&event.logical_key, self.input.mods)).flatten();
                if let Some(key) = &mut logical {
                    key.mods = modifiers(self.input.mods);
                    if key.mods.cmd && let crate::Key::Text(text) = &key.key && text.chars().count() == 1 {
                        key.key = crate::Key::Command(text.chars().next().unwrap().to_ascii_lowercase());
                    }
                }
                activate = logical.as_ref().is_some_and(|k| k.key == crate::Key::Tab);
                if let winit::keyboard::PhysicalKey::Code(key) = event.physical_key {
                    self.rt.physical_with_fallback(crate::PhysicalInput::Key { key, mods: modifiers(self.input.mods),
                        pressed: event.state == ElementState::Pressed, repeat: event.repeat }, logical);
                } else if let Some(key) = logical { self.rt.key(key); }
            }
            WindowEvent::Touch(touch) => {
                let point = (touch.location.x as f32 / scale, touch.location.y as f32 / scale);
                match touch.phase {
                    TouchPhase::Started if self.input.touch.is_none() => {
                        self.input.touch = Some(Touch { id: touch.id, scroll: self.rt.web_touch_start(point.0, point.1),
                            origin: point, last: point, at: Instant::now(), velocity: 0.0, pan: false, horizontal: false });
                    }
                    TouchPhase::Moved => {
                        let Some(t) = &mut self.input.touch else { return false };
                        if t.id != touch.id { return false; }
                        let (dx, dy) = (point.0 - t.origin.0, point.1 - t.origin.1);
                        if !t.pan && dx.hypot(dy) > 8.0 {
                            // The pan's first direction picks the axis for the rest of the touch.
                            t.pan = true;
                            (t.scroll, t.horizontal) = self.rt.web_touch_pan(t.origin.0, t.origin.1, dx.abs() > dy.abs(), t.scroll);
                        }
                        let horizontal = t.horizontal;
                        let along = move |p: (f32, f32)| if horizontal { p.0 } else { p.1 };
                        let now = Instant::now();
                        let dt = now.duration_since(t.at).as_secs_f32();
                        if dt > 0.0 { t.velocity = (along(point) - along(t.last)) / dt; }
                        if t.pan { if let Some(id) = t.scroll { self.rt.web_touch_move(id, along(point) - along(t.origin)); } }
                        t.last = point; t.at = now;
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if self.input.touch.as_ref().is_none_or(|t| t.id != touch.id) { return false; }
                        let t = self.input.touch.take().unwrap();
                        let velocity = if t.pan && touch.phase == TouchPhase::Ended && t.at.elapsed().as_millis() < 100 { t.velocity } else { 0.0 };
                        self.rt.web_touch_end(t.scroll, velocity);
                        if !t.pan && touch.phase == TouchPhase::Ended {
                            self.rt.pointer_down(point.0, point.1);
                            self.rt.pointer_up(point.0, point.1);
                            activate = true;
                            immediate = true;
                        }
                        self.rt.pointer_leave();
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
        // Moves and wheels are coalesced by rAF. Only activation-sensitive input must
        // commit before the DOM handler returns (clipboard, links and keyboard focus).
        if immediate {
            self.rt.frame(0.0);
            self.clipboard.flush(&mut self.rt);
            self.sync_input(activate);
        }
        true
    }
}

fn modifiers(mods: ModifiersState) -> crate::Mods {
    crate::Mods { shift: mods.shift_key(), alt: mods.alt_key(), ctrl: mods.control_key(),
        logo: mods.super_key(), cmd: mods.super_key() || mods.control_key() }
}
