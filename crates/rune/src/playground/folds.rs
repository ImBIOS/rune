//! Which groups the list folds: kept while the app runs, and through [`Playground::folds`]'
//! hooks from one run to the next.

use super::*;

/// [`Playground::folds`]' hooks. `load` runs once, when the playground first applies a request.
#[derive(Default)]
pub(super) struct Folds {
    load: Option<Box<dyn FnOnce() -> Vec<String>>>,
    save: Option<Box<dyn Fn(&[String])>>,
}

impl<S: 'static> Playground<S> {
    /// Keeps the folded groups from one run to the next. Without it they stay folded while the
    /// app runs, closing and opening the playground included. `load` gives the names of the
    /// groups folded last time, once, as the playground first opens; names it no longer has are
    /// dropped. `save` gets the folded groups' names each time one folds or unfolds. With
    /// [`Prefs`](crate::prefs::Prefs):
    ///
    /// ```ignore
    /// .folds(
    ///     || Prefs::open(APP_ID).get("playground_folds").unwrap_or_default(),
    ///     |groups| { Prefs::open(APP_ID).set("playground_folds", &groups).ok(); },
    /// )
    /// ```
    pub fn folds(mut self, load: impl Fn() -> Vec<String> + 'static, save: impl Fn(&[String]) + 'static) -> Self {
        self.folds = Folds { load: Some(Box::new(load)), save: Some(Box::new(save)) };
        self
    }
}

impl<S: 'static> Live<S> {
    /// Folds the groups [`Playground::folds`]' `load` names, the first time it's called.
    pub(super) fn load_folds(&mut self) {
        let Some(load) = self.folds.load.take() else { return };
        let names = load();
        let mut groups: Vec<&'static str> = self.order.iter().map(|&i| self.group(i)).collect();
        groups.dedup();
        self.folded = groups.into_iter().filter(|g| names.iter().any(|n| n == g)).collect();
    }

    /// Changes which groups are folded, and saves them if that changed them.
    pub(super) fn refold(&mut self, change: impl FnOnce(&mut Vec<&'static str>)) {
        let before = self.folded.clone();
        change(&mut self.folded);
        if self.folded == before { return; }
        if let Some(save) = &self.folds.save {
            save(&self.folded.iter().map(|g| (*g).to_owned()).collect::<Vec<_>>());
        }
    }
}
