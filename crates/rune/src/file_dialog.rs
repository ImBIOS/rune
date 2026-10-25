//! Inject native dialogs in the shell, scripted responses in headless fixtures.
//! Native futures use rfd on macOS and Windows, and the XDG desktop portal's FileChooser
//! over zbus on Linux.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(all(feature = "native-file-dialog", target_os = "linux"))]
mod portal;
#[cfg(any(all(feature = "native-file-dialog", target_os = "linux"), all(test, unix)))]
mod portal_format;
#[cfg(all(feature = "native-file-dialog", target_os = "linux"))]
pub(crate) use portal::window_focus;
#[cfg(all(feature = "native-file-dialog", target_os = "linux"))]
use portal::show as native;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDialogOptions {
    pub title: String,
    pub directory: Option<PathBuf>,
    pub file_name: Option<String>,
    pub filters: Vec<(String, Vec<String>)>,
}
impl FileDialogOptions {
    pub fn filter(mut self, name: impl Into<String>, extensions: &[&str]) -> Self {
        self.filters.push((name.into(), extensions.iter().map(|s| (*s).into()).collect())); self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileDialogKind { Open, Save }

/// Expected request and its response. `None` is cancellation, not a missing fixture.
#[derive(Clone, Debug)]
pub struct ScriptedFileDialog {
    pub kind: FileDialogKind,
    pub path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct FileDialogs(Backend);
#[derive(Clone)]
enum Backend {
    #[cfg(feature = "native-file-dialog")]
    Native,
    Scripted(Arc<Mutex<VecDeque<ScriptedFileDialog>>>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDialogError(pub String);
impl std::fmt::Display for FileDialogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for FileDialogError {}

impl FileDialogs {
    /// Native dialogs are opt-in at the app's capability boundary. Headless apps inject
    /// `scripted` instead, so tests never depend on filesystem UI or a display server.
    /// Poll asynchronously: blocking the window's main thread (for example with
    /// `pollster::block_on`) deadlocks Cocoa's dialog presentation. On Linux, `open` and
    /// `save` return an error when the desktop has no XDG portal FileChooser.
    #[cfg(feature = "native-file-dialog")]
    pub fn native() -> Self { Self(Backend::Native) }
    pub fn scripted(responses: impl IntoIterator<Item = ScriptedFileDialog>) -> Self {
        Self(Backend::Scripted(Arc::new(Mutex::new(responses.into_iter().collect()))))
    }
    pub async fn open(&self, options: FileDialogOptions) -> Result<Option<PathBuf>, FileDialogError> {
        self.show(FileDialogKind::Open, options).await
    }
    pub async fn save(&self, options: FileDialogOptions) -> Result<Option<PathBuf>, FileDialogError> {
        self.show(FileDialogKind::Save, options).await
    }
    async fn show(&self, kind: FileDialogKind, options: FileDialogOptions) -> Result<Option<PathBuf>, FileDialogError> {
        match &self.0 {
            Backend::Scripted(responses) => {
                let mut responses = responses.lock().unwrap();
                let response = responses.front().ok_or_else(|| FileDialogError("file dialog fixture exhausted".into()))?;
                if response.kind != kind {
                    return Err(FileDialogError(format!("expected {:?} dialog, received {kind:?}", response.kind)));
                }
                let _ = options;
                Ok(responses.pop_front().unwrap().path)
            }
            #[cfg(feature = "native-file-dialog")]
            Backend::Native => native(kind, options).await,
        }
    }
}

#[cfg(all(feature = "native-file-dialog", any(target_os = "macos", target_os = "windows")))]
async fn native(kind: FileDialogKind, options: FileDialogOptions) -> Result<Option<PathBuf>, FileDialogError> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(options.title);
    if let Some(directory) = options.directory { dialog = dialog.set_directory(directory); }
    if let Some(file_name) = options.file_name { dialog = dialog.set_file_name(file_name); }
    for (name, extensions) in options.filters { dialog = dialog.add_filter(name, &extensions); }
    let file = match kind {
        FileDialogKind::Open => dialog.pick_file().await,
        FileDialogKind::Save => dialog.save_file().await,
    };
    Ok(file.map(|file| file.path().to_path_buf()))
}
#[cfg(all(feature = "native-file-dialog", not(any(target_os = "linux", target_os = "macos", target_os = "windows"))))]
async fn native(_: FileDialogKind, _: FileDialogOptions) -> Result<Option<PathBuf>, FileDialogError> {
    Err(FileDialogError("native file dialogs aren't supported on this platform".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scripted_dialogs_preserve_order_unicode_paths_and_cancellation() {
        let path = PathBuf::from("/fixture/录音.wav");
        let dialogs = FileDialogs::scripted([
            ScriptedFileDialog { kind: FileDialogKind::Open, path: Some(path.clone()) },
            ScriptedFileDialog { kind: FileDialogKind::Save, path: None },
        ]);
        let options = FileDialogOptions::default().filter("Audio", &["wav", "flac"]);
        assert!(pollster::block_on(dialogs.save(options.clone())).is_err());
        assert_eq!(pollster::block_on(dialogs.open(options.clone())).unwrap(), Some(path));
        assert_eq!(pollster::block_on(dialogs.clone().save(options.clone())).unwrap(), None);
        assert!(pollster::block_on(dialogs.open(options)).is_err());
    }
}
