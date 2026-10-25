//! A window's split layout: a resizable, collapsible sidebar, the main pane under a toolbar,
//! and an optional inspector, with the title bar region cleared for the traffic lights.

use rune::{Anim, AnimExt, Color, Cursor, Drag, El, Phase, Spring, Tx, col, el, row};

use super::pane::PaneState;
use super::toolbar::{TOOLBAR_HEIGHT, Toolbar};
use crate::{IconButton, IconButtonVariant, Kit};

type Apply = Box<dyn FnOnce(El) -> El>;

/// The gap around the main pane's card.
const GUTTER: f32 = 8.0;
/// The resize grip's width, straddling nothing: it sits inside the pane's edge.
const GRIP: f32 = 6.0;
/// The sidebar toggle: a 24 point button, 8 points past the traffic lights.
const TOGGLE: f32 = 24.0;

struct Pane {
    state: PaneState,
    content: El,
    handlers: Vec<Apply>,
    toggle: Option<Apply>,
}

/// The app shell: `SplitView::new(main).sidebar(..).toolbar(..).inspector(..)
/// .titlebar_inset(cx.titlebar_inset()).build(&kit)`.
///
/// The sidebar and inspector are flush panes on the window's deep background; the main pane is
/// a rounded card beside them. A 48 point band across the top is the title bar: it drags the
/// window, its leading end clears the traffic lights (pass [`Cx::titlebar_inset`], 0 in full
/// screen), and the sidebar's toggle button sits just past them. Panes resize from their
/// inner edge (double-click resets the width, dragging below half the minimum collapses) and
/// open and close on the kit's disclosure spring, faster for narrow panes. Under Reduce Motion
/// they snap. Widths and open states live in the app's [`PaneState`]s.
///
/// The band's middle is 24 points down: place the traffic lights there with
/// `Chrome { traffic_lights: Some((16.0, 17.0)), .. }`.
///
/// [`Cx::titlebar_inset`]: rune::Cx::titlebar_inset
pub struct SplitView {
    main: El,
    sidebar: Option<Pane>,
    inspector: Option<Pane>,
    toolbar: Option<Toolbar>,
    inset: f32,
    show_toggle: bool,
}

impl SplitView {
    pub fn new(main: El) -> SplitView {
        SplitView { main, sidebar: None, inspector: None, toolbar: None, inset: 0.0, show_toggle: true }
    }
    /// The leading pane. `access` finds its state in the app's; the grip and the toggle button
    /// write to it. `content` is usually a built `Sidebar`.
    pub fn sidebar<S: 'static>(mut self, state: &PaneState, access: fn(&mut S) -> &mut PaneState, content: El) -> Self {
        self.sidebar = Some(pane(*state, access, content, 1.0, true));
        self
    }
    /// The trailing pane, resized from its leading edge.
    pub fn inspector<S: 'static>(mut self, state: &PaneState, access: fn(&mut S) -> &mut PaneState, content: El) -> Self {
        self.inspector = Some(pane(*state, access, content, -1.0, false));
        self
    }
    /// The band above the main pane (and the inspector). Without one the band is empty and
    /// still drags the window.
    pub fn toolbar(mut self, t: Toolbar) -> Self {
        self.toolbar = Some(t);
        self
    }
    /// Points the traffic lights take at the window's leading edge: `Cx::titlebar_inset`.
    pub fn titlebar_inset(mut self, points: f32) -> Self {
        self.inset = points.max(0.0);
        self
    }
    /// Leaves out the sidebar's toggle button (the app shows its own, or none).
    pub fn hide_sidebar_toggle(mut self) -> Self {
        self.show_toggle = false;
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let c = &k.c;
        let sidebar_open = self.sidebar.as_ref().is_some_and(|p| p.state.is_open());
        let inspector_open = self.inspector.as_ref().is_some_and(|p| p.state.is_open());
        let lights = if self.inset > 0.0 { self.inset + GUTTER } else { 12.0 };
        let toggle = self.show_toggle && self.sidebar.is_some();
        let past_toggle = if toggle { lights + TOGGLE + GUTTER } else { lights };

        let mut overlay = None;
        let sidebar = self.sidebar.map(|mut p| {
            if let Some(t) = p.toggle.take().filter(|_| toggle) {
                let button = IconButton::new(&super::SIDEBAR)
                    .variant(IconButtonVariant::GhostMuted)
                    .label(if sidebar_open { "Hide sidebar" } else { "Show sidebar" })
                    .build(k);
                overlay = Some(t(button).key("sidebar-toggle").absolute(lights, (TOOLBAR_HEIGHT - TOGGLE) / 2.0));
            }
            let band = el().key("sidebar-titlebar").w_full().h(TOOLBAR_HEIGHT).window_drag();
            dock(k, "sidebar-pane", p, true, Some(band))
        });
        let toolbar = self
            .toolbar
            .unwrap_or_default()
            .padding(if sidebar_open { 12.0 } else { past_toggle })
            .build(k);
        let card = col()
            .key("main-pane")
            .grow(1.0)
            .min_w(0.0)
            .min_h(0.0)
            .ml(if sidebar_open { 0.0 } else { GUTTER })
            .mr(if inspector_open { 0.0 } else { GUTTER })
            .mb(GUTTER)
            .fill(c.bg_base)
            .radius(k.radius.lg)
            .border(1.0, c.border_muted)
            .clip_rounded(k.radius.lg)
            .motion(k.motion.disclosure)
            .child(self.main.grow(1.0).min_h(0.0));
        let mut below = row().key("main-row").w_full().grow(1.0).min_h(0.0).child(card);
        if let Some(p) = self.inspector {
            below = below.child(dock(k, "inspector-pane", p, false, None));
        }
        let main = col().key("main-column").grow(1.0).min_w(0.0).h_full().child(toolbar).child(below);
        let mut split = row().key("split").w_full().h_full();
        if let Some(s) = sidebar {
            split = split.child(s);
        }
        let mut root = el().key("shell").w_full().h_full().fill(c.bg_deep).child(split.child(main));
        if let Some(o) = overlay {
            root = root.child(o);
        }
        root
    }
}

/// Handlers for a pane's grip and toggle, typed by the app's state.
fn pane<S: 'static>(state: PaneState, access: fn(&mut S) -> &mut PaneState, content: El, sign: f32, toggle: bool) -> Pane {
    let drag: Apply = Box::new(move |e: El| {
        e.on_drag(move |s: &mut S, d: &mut Drag, tx: &mut Tx| {
            let p = access(s);
            match d.phase {
                Phase::Down => {
                    d.data = p.width();
                    p.set_resizing(true);
                }
                Phase::Move => {
                    tx.instant();
                    p.drag_to(d.data + sign * d.dx);
                }
                Phase::Up => p.set_resizing(false),
            }
        })
        .on_double_press(move |s: &mut S| access(s).reset_width())
    });
    let toggle: Option<Apply> =
        toggle.then(|| Box::new(move |e: El| e.on_press(move |s: &mut S| access(s).toggle())) as Apply);
    Pane { state, content, handlers: vec![drag], toggle }
}

/// The pane's slot: its width springs between 0 and the pane's width while the content keeps
/// its full width, clipped, pinned to the outer edge. The content fades out late in a close
/// and back in early in an open.
fn dock(k: &Kit, key: &str, p: Pane, leading: bool, band: Option<El>) -> El {
    let s = p.state;
    let spring = pane_spring(k.motion.disclosure, s.width());
    let fade: Anim = if s.is_open() { Spring::new(spring.duration * 0.5, 0.0).into() } else { k.motion.control.delay(spring.duration * 0.6) };
    let body = col()
        .key("pane-content")
        .w(s.width())
        .h_full()
        .opacity(if s.is_open() { 1.0 } else { 0.0 })
        .opacity_motion(fade)
        .children(band)
        .child(col().key("pane-body").w_full().grow(1.0).min_h(0.0).child(p.content.w_full().h_full()));
    let body = if leading { body.pin(Some(0.0), Some(0.0), None, Some(0.0)) } else { body.pin(None, Some(0.0), Some(0.0), Some(0.0)) };
    let mut slot = el().key(key).w(s.shown_width()).h_full().clip_soft(0.0).motion(spring).child(body);
    if s.is_open() || s.is_resizing() {
        let mut grip = el()
            .key(if leading { "sidebar-grip" } else { "inspector-grip" })
            .role("separator")
            .label(if leading { "Resize sidebar" } else { "Resize inspector" })
            .w(GRIP)
            .cursor(Cursor::ResizeColumn)
            .fill(Color::TRANSPARENT);
        grip = if leading { grip.pin(None, Some(0.0), Some(0.0), Some(0.0)) } else { grip.pin(Some(0.0), Some(0.0), None, Some(0.0)) };
        for h in p.handlers {
            grip = h(grip);
        }
        slot = slot.child(grip);
    }
    slot
}

/// `base` scaled for a pane `width` wide: √(width / 480) of its duration, at least 0.15 s
/// (or `base`'s own, if shorter), so narrow panes don't drift.
fn pane_spring(base: Spring, width: f32) -> Spring {
    let floor = base.duration.min(0.15);
    Spring::new((base.duration * (width / 480.0).max(0.0).sqrt()).clamp(floor, base.duration), base.bounce)
}
