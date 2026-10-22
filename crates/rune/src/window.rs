//! The prototype shell: winit, one window at a time, a wgpu surface, redraws only while
//! something animates. Keys, IME and the clipboard route to the runtime; other threads wake it
//! through an `EventLoopProxy` (live data, `AppHandle`). The runtime outlives the window
//! (`App`): closing hides or destroys the window and keeps the state, GPU device and renderer.

use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, Ime as WIme, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme, Window, WindowId};

use crate::app::{App, Shared, WindowRequest};
use crate::edit::{Ime, Key, KeyEvent, Mods};
use crate::el::{Cursor, Shell, Tx};
use crate::render::{Gpu, Renderer};
use crate::runtime::{Env, Runtime, Waker};
use crate::tooling::{AppSpec, Args, runtime_for};

/// The open window. Fields drop in order: the surface before its window.
struct Win {
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    activator: Option<crate::activation::Activator>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    window: Arc<Window>,
    /// False while hidden (platforms that can hide a window keep it for the next open).
    visible: bool,
    occluded: bool,
    cursor: Cursor,
}

enum WakeEvent { App, Live }

struct Host<S: 'static> {
    spec: AppSpec<S>,
    args: Args,
    keep_running: bool,
    on_close: Option<fn(&mut S, &mut Tx)>,
    release_gpu: bool,
    open_at_start: bool,
    shared: Arc<Shared<S>>,
    rt: Runtime<S>,
    win: Option<Win>,
    gpu: Option<(Gpu, Renderer, wgpu::TextureFormat)>,
    /// A window has shown before: the next one opens settled.
    shown: bool,
    started: bool,
    last: Instant,
    idle: bool,
    mods: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
}

pub fn run<S: 'static>(app: App<S>, args: Args) {
    let event_loop = EventLoop::<WakeEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let live_proxy = proxy.clone();
    let waker: Waker = Arc::new(move || {
        let _ = proxy.send_event(WakeEvent::App);
    });
    let (open, keep_running, on_close, release_gpu) = (app.open, app.keep_running, app.on_close, app.release_gpu);
    let (spec, state, shared) = app.into_parts(args.scene.as_deref());
    let size = args.size.unwrap_or(spec.size);
    // The window reports the system appearance and scale when it opens.
    let env = Env { system_dark: args.theme.as_deref() != Some("light"), scale: 1.0 };
    let mut rt = runtime_for(&spec, state, size, env, waker.clone());
    rt.set_live_waker(Arc::new(move || { let _ = live_proxy.send_event(WakeEvent::Live); }));
    rt.set_live_visible(false);
    rt.wall_start = Some(Instant::now());
    shared.connect(waker);
    let mut host = Host {
        spec,
        args,
        keep_running,
        on_close,
        release_gpu,
        open_at_start: open,
        shared,
        rt,
        win: None,
        gpu: None,
        shown: false,
        started: false,
        last: Instant::now(),
        idle: true,
        mods: ModifiersState::empty(),
        clipboard: arboard::Clipboard::new().ok(),
    };
    event_loop.run_app(&mut host).expect("run");
}


fn is_wayland(event_loop: &ActiveEventLoop) -> bool {
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    return winit::platform::wayland::ActiveEventLoopExtWayland::is_wayland(event_loop);
    #[cfg(not(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    {
        let _ = event_loop;
        false
    }
}

fn cursor_icon(c: Cursor) -> CursorIcon {
    match c {
        Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text => CursorIcon::Text,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Move => CursorIcon::Move,
        Cursor::ResizeColumn => CursorIcon::ColResize,
        Cursor::ResizeRow => CursorIcon::RowResize,
    }
}

fn map_key(key: &WKey, m: ModifiersState) -> Option<KeyEvent> {
    let cmd = if cfg!(target_os = "macos") { m.super_key() } else { m.control_key() };
    let mods = Mods { shift: m.shift_key(), cmd, alt: m.alt_key(), ctrl: m.control_key(), logo: m.super_key() };
    let key = match key {
        WKey::Named(n) => match n {
            NamedKey::Enter => Key::Enter,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::Escape => Key::Escape,
            NamedKey::Tab => Key::Tab,
            NamedKey::Space => Key::Text(" ".into()),
            NamedKey::F1 => Key::Function(1),
            NamedKey::F2 => Key::Function(2),
            NamedKey::F3 => Key::Function(3),
            NamedKey::F4 => Key::Function(4),
            NamedKey::F5 => Key::Function(5),
            NamedKey::F6 => Key::Function(6),
            NamedKey::F7 => Key::Function(7),
            NamedKey::F8 => Key::Function(8),
            NamedKey::F9 => Key::Function(9),
            NamedKey::F10 => Key::Function(10),
            NamedKey::F11 => Key::Function(11),
            NamedKey::F12 => Key::Function(12),
            NamedKey::F13 => Key::Function(13),
            NamedKey::F14 => Key::Function(14),
            NamedKey::F15 => Key::Function(15),
            NamedKey::F16 => Key::Function(16),
            NamedKey::F17 => Key::Function(17),
            NamedKey::F18 => Key::Function(18),
            NamedKey::F19 => Key::Function(19),
            NamedKey::F20 => Key::Function(20),
            NamedKey::F21 => Key::Function(21),
            NamedKey::F22 => Key::Function(22),
            NamedKey::F23 => Key::Function(23),
            NamedKey::F24 => Key::Function(24),
            _ => return None,
        },
        WKey::Character(c) => {
            if cmd {
                Key::Command(c.chars().next()?.to_ascii_lowercase())
            } else {
                Key::Text(c.to_string())
            }
        }
        _ => return None,
    };
    Some(KeyEvent { key, mods })
}

impl<S: 'static> Host<S> {
    fn visible(&self) -> bool {
        self.win.as_ref().is_some_and(|w| w.visible)
    }

    /// Applies everything other threads asked for since the last wake-up.
    fn drain(&mut self, event_loop: &ActiveEventLoop) {
        let requests = self.shared.take();
        for f in requests.updates {
            self.rt.update(f);
        }
        if requests.quit {
            event_loop.exit();
            return;
        }
        match requests.window {
            Some(WindowRequest::Open(token)) => self.open(event_loop, token),
            Some(WindowRequest::Close) => self.close(event_loop),
            None => {}
        }
        self.shell(event_loop);
        if let Some(w) = self.win.as_ref().filter(|w| w.visible) {
            w.window.request_redraw();
        }
    }

    /// Carries out what a handler or update asked with `Tx::close_window` / `Tx::quit`.
    fn shell(&mut self, event_loop: &ActiveEventLoop) {
        match self.rt.shell_request.take() {
            Some(Shell::Quit) => event_loop.exit(),
            Some(Shell::Close) => self.close(event_loop),
            None => {}
        }
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        if !self.visible() {
            return;
        }
        if let Some(f) = self.on_close {
            self.rt.update(f);
        }
        self.rt.set_live_visible(false);
        if !self.keep_running || self.rt.shell_request.take() == Some(Shell::Quit) {
            event_loop.exit();
            return;
        }
        self.rt.pointer_leave();
        self.mods = ModifiersState::empty();
        if self.release_gpu {
            self.win = None;
            self.gpu = None;
            self.rt.text.atlas.invalidate();
        } else if is_wayland(event_loop) {
            // Wayland has no hidden toplevel: unmap by destroying, recreate on open.
            self.win = None;
        } else if let Some(w) = self.win.as_mut() {
            w.window.set_visible(false);
            w.visible = false;
        }
    }

    fn open(&mut self, event_loop: &ActiveEventLoop, token: Option<String>) {
        // The launcher's startup token belongs to the first window only.
        #[allow(unused_mut)]
        let mut from_env = None;
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if !self.shown {
            use winit::platform::startup_notify::{EventLoopExtStartupNotify, reset_activation_token_env};
            from_env = event_loop.read_token_from_env().map(|t| t.into_raw());
            reset_activation_token_env();
        }
        let token = token.or(from_env);
        let Some(w) = self.win.as_mut() else {
            self.create(event_loop, token);
            return;
        };
        if !w.visible {
            w.window.set_visible(true);
            w.visible = true;
            self.rt.settle_all();
            self.idle = true;
        }
        if w.window.is_minimized() == Some(true) {
            w.window.set_minimized(false);
        }
        w.occluded = false;
        self.rt.set_live_visible(true);
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if is_wayland(event_loop) {
            match token {
                Some(token) => {
                    if w.activator.is_none() {
                        w.activator = crate::activation::Activator::new(&w.window);
                    }
                    if let Some(a) = w.activator.as_mut() {
                        a.activate(&token);
                    }
                }
                // Without a token the compositor may only mark the window urgent.
                None => w.window.request_user_attention(Some(winit::window::UserAttentionType::Informational)),
            }
            w.window.request_redraw();
            return;
        }
        let _ = token;
        w.window.focus_window();
        w.window.request_redraw();
    }

    #[cfg_attr(not(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))), allow(unused_variables))]
    fn create(&mut self, event_loop: &ActiveEventLoop, token: Option<String>) {
        let size = self.args.size.unwrap_or(self.spec.size);
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title(self.spec.title)
            .with_inner_size(LogicalSize::new(size.0 as f64, size.1 as f64))
            .with_min_inner_size(LogicalSize::new(420.0, 400.0));
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        {
            if let Some(id) = crate::tooling::app_id() {
                attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, id, id);
                attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, id, id);
            }
            if let Some(token) = token {
                let token = winit::window::ActivationToken::from_raw(token);
                attrs = winit::platform::startup_notify::WindowAttributesExtStartupNotify::with_activation_token(attrs, token);
            }
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs.with_titlebar_transparent(true).with_title_hidden(true).with_fullsize_content_view(true);
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        window.set_ime_allowed(true);
        let (gpu, surface) = match self.gpu.take() {
            Some((gpu, renderer, format)) => {
                let surface = gpu.instance.create_surface(window.clone()).expect("surface");
                self.gpu = Some((gpu, renderer, format));
                (None, surface)
            }
            None => {
                let instance = Gpu::instance();
                let surface = instance.create_surface(window.clone()).expect("surface");
                (Some(Gpu::new(instance, Some(&surface))), surface)
            }
        };
        let adapter = &gpu.as_ref().or(self.gpu.as_ref().map(|g| &g.0)).unwrap().adapter;
        let phys = window.inner_size();
        let mut config = surface.get_default_config(adapter, phys.width.max(1), phys.height.max(1)).expect("surface config");
        let caps = surface.get_capabilities(adapter);
        config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 2;
        if let Some(gpu) = gpu {
            let renderer = Renderer::new(&gpu.device, config.format);
            self.gpu = Some((gpu, renderer, config.format));
        }
        let (gpu, renderer, format) = self.gpu.as_mut().unwrap();
        if *format != config.format {
            *renderer = Renderer::new(&gpu.device, config.format);
            *format = config.format;
            self.rt.text.atlas.invalidate();
        }
        surface.configure(&gpu.device, &config);
        let scale = window.scale_factor() as f32;
        let dark = match self.args.theme.as_deref() {
            Some("light") => false,
            Some("dark") => true,
            _ => window.theme().is_none_or(|t| t == Theme::Dark),
        };
        self.rt.set_system_dark(dark);
        self.rt.set_viewport((phys.width as f32 / scale, phys.height as f32 / scale), scale);
        if self.shown {
            // Reopening paints the settled state in the first frame (guarantee 8).
            self.rt.settle_all();
        }
        self.shown = true;
        self.idle = true;
        self.rt.set_live_visible(true);
        window.request_redraw();
        self.win = Some(Win {
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            activator: None,
            surface,
            config,
            window,
            visible: true,
            occluded: false,
            cursor: Cursor::Default,
        });
    }

    fn redraw(&mut self) {
        let (Some(l), Some((gpu, renderer, _))) = (self.win.as_mut(), self.gpu.as_mut()) else { return };
        if !l.visible || l.occluded || l.window.is_minimized().unwrap_or(false) {
            return;
        }
        // Acquire before latching live values so a drawable wait cannot age the input.
        let frame = match l.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                l.surface.configure(&gpu.device, &l.config);
                l.window.request_redraw();
                return;
            }
            _ => { l.window.request_redraw(); return; }
        };
        let now = Instant::now();
        // After an idle stretch, start the new motion from its first pose rather than
        // skipping ahead by the time spent idle.
        let dt = if self.idle { 1.0 / 120.0 } else { (now - self.last).as_secs_f64().min(1.0 / 30.0) };
        self.last = now;
        let rt = &mut self.rt;
        let animating = rt.frame(dt);
        let cursor = rt.cursor();
        if cursor != l.cursor {
            l.cursor = cursor;
            l.window.set_cursor(cursor_icon(cursor));
        }
        if let Some(c) = rt.copy_request.take()
            && let Some(cb) = self.clipboard.as_mut()
        {
            let _ = cb.set_text(c);
        }
        if let Some(a) = rt.ime_area {
            l.window.set_ime_cursor_area(
                LogicalPosition::new(a[0] as f64, a[1] as f64),
                LogicalSize::new(a[2] as f64, a[3] as f64),
            );
        }
        let view = frame.texture.create_view(&Default::default());
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (l.config.width, l.config.height));
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.draw(&mut enc, &view, rt.clear_color());
        gpu.queue.submit([enc.finish()]);
        l.window.pre_present_notify();
        gpu.queue.present(frame);
        self.idle = !(animating || rt.has_pending_input() || rt.live_pending());
        if !self.idle {
            l.window.request_redraw();
        }
    }
}

impl<S: 'static> ApplicationHandler<WakeEvent> for Host<S> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        if self.open_at_start {
            self.open(event_loop, None);
        }
        self.started = true;
        self.drain(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        if !self.started { return; }
        match event {
            WakeEvent::App => self.drain(event_loop),
            WakeEvent::Live => {
                if self.rt.live_pending() && let Some(w) = self.win.as_ref().filter(|w| w.visible && !w.occluded) {
                    w.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.win.as_ref().is_none_or(|w| w.window.id() != id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.close(event_loop);
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                self.shell(event_loop);
                return;
            }
            _ => {}
        }
        let (Some(l), Some((gpu, ..))) = (self.win.as_mut(), self.gpu.as_ref()) else { return };
        let rt = &mut self.rt;
        let scale = l.window.scale_factor() as f32;
        match event {
            WindowEvent::Resized(size) => {
                rt.set_live_visible(l.visible && !l.occluded && size.width > 0 && size.height > 0 && !l.window.is_minimized().unwrap_or(false));
                if size.width == 0 || size.height == 0 { self.idle = true; return; }
                l.config.width = size.width.max(1);
                l.config.height = size.height.max(1);
                l.surface.configure(&gpu.device, &l.config);
                rt.set_viewport((size.width as f32 / scale, size.height as f32 / scale), scale);
                l.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => l.window.request_redraw(),
            WindowEvent::Occluded(occluded) => {
                l.occluded = occluded;
                rt.set_live_visible(l.visible && !occluded && !l.window.is_minimized().unwrap_or(false));
                if occluded { self.idle = true; return; }
                // Coming back paints the settled state in the first frame (guarantee 8).
                rt.settle_all();
                l.window.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                if self.args.theme.is_none() {
                    rt.set_system_dark(theme == Theme::Dark);
                    l.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } if event.state == ElementState::Pressed => {
                if let Some(ev) = map_key(&event.logical_key, self.mods) {
                    if ev.key == Key::Command('v') && rt.has_focus() {
                        if let Some(text) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) {
                            rt.paste(text);
                        }
                    } else {
                        rt.key(ev);
                    }
                    l.window.request_redraw();
                }
            }
            WindowEvent::Ime(ime) => {
                match ime {
                    WIme::Preedit(s, cur) => rt.ime(Ime::Preedit(s, cur)),
                    WIme::Commit(s) => rt.ime(Ime::Commit(s)),
                    _ => {}
                }
                l.window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some((x, y)) = rt.pointer() {
                    let dy = match delta {
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / scale,
                        MouseScrollDelta::LineDelta(_, ly) => ly * 40.0,
                    };
                    rt.wheel(x, y, dy);
                    l.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                rt.pointer_move(position.x as f32 / scale, position.y as f32 / scale);
                l.window.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                rt.pointer_leave();
                l.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if let Some((x, y)) = rt.pointer() {
                    match state {
                        ElementState::Pressed => rt.pointer_down(x, y),
                        ElementState::Released => rt.pointer_up(x, y),
                    }
                }
                l.window.request_redraw();
            }
            _ => {}
        }
    }
}
