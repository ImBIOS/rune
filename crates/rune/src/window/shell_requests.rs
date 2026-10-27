//! What handlers and updates ask of the window itself, carried out by `Host::shell`:
//! minimizing it.
use super::*;

fn minimize_ignored(what: &str) {
    if cfg!(debug_assertions) { eprintln!("rune: minimize ignored: {what} can't minimize"); }
}

impl<S: 'static> Host<S> {
    /// `Tx::minimize` and `AppHandle::minimize`: miniaturize on macOS, `set_minimized`
    /// elsewhere. A hidden window, a panel or launcher, and a window that can't minimize
    /// ignore it.
    pub(super) fn minimize(&mut self) {
        #[cfg(target_os = "macos")]
        let launcher = self.launcher();
        let Some(w) = self.win.as_mut().filter(|w| w.visible) else { return minimize_ignored("a hidden window") };
        #[cfg(target_os = "macos")]
        if w.panel.is_some() || launcher { return minimize_ignored("a panel"); }
        // Off macOS winit reports a minimize button on a borderless window too.
        if self.options.borderless || !w.window.enabled_buttons().contains(winit::window::WindowButtons::MINIMIZE) {
            return minimize_ignored("a window without a minimize button");
        }
        #[cfg(target_os = "macos")]
        { w.prime = 0; }
        w.window.set_minimized(true);
    }
}
