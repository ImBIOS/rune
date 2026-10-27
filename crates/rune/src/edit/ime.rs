//! Input method events: what the platform's input method (dead keys, CJK, the emoji picker,
//! dictation) composes for the focused `text_input`.
use zeroize::Zeroize;

#[derive(Clone, Debug, PartialEq)]
pub enum Ime {
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
}

impl Drop for Ime {
    fn drop(&mut self) {
        match self { Self::Preedit(text, _) | Self::Commit(text) => text.zeroize() }
    }
}
