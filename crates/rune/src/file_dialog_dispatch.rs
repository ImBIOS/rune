use super::*;
use crate::file_dialog::{FileDialogError, FileDialogKind, FileDialogOptions, FileDialogResult, Pending, Request};

impl<S: 'static> Runtime<S> {
    /// The file dialog showing, or about to show, from [`Tx::open_file`] or [`Tx::save_file`].
    pub fn file_dialog(&self) -> Option<(FileDialogKind, &FileDialogOptions)> {
        self.file_dialog.as_ref().map(|d| (d.kind, &d.options))
    }

    /// Answers the file dialog showing as if the user chose `path`, or cancelled with `None`.
    /// Its callback runs on the next frame. For tests and headless drivers; native dialogs
    /// answer themselves.
    pub fn resolve_file_dialog(&mut self, path: Option<std::path::PathBuf>) -> Result<(), FileDialogError> {
        let id = self.file_dialog.as_ref().ok_or_else(|| FileDialogError("no file dialog is open".into()))?.id;
        self.answer_file_dialog(id, Ok(path));
        Ok(())
    }

    /// Takes a handler's request. One dialog at a time: a request while one is open is
    /// answered with an error rather than queued, so a double click opens one dialog.
    pub(crate) fn request_file_dialog(&mut self, request: Request) {
        self.next_file_dialog += 1;
        let id = self.next_file_dialog;
        self.file_dialog_answers.push((id, request.on_answer));
        if self.file_dialog.is_some() {
            self.trace_event("file-dialog:busy");
            self.queue.push(Action::FileDialog(id, Err(FileDialogError("a file dialog is already open".into()))));
            return;
        }
        self.trace_event(format_args!("file-dialog:open:{:?}", request.kind));
        self.file_dialog = Some(Pending { id, kind: request.kind, options: request.options, shown: false });
    }

    /// The dialog for the host to show, marked shown. Nothing while it shows.
    pub(crate) fn take_file_dialog(&mut self) -> Option<(u64, FileDialogKind, FileDialogOptions)> {
        let d = self.file_dialog.as_mut().filter(|d| !d.shown)?;
        d.shown = true;
        Some((d.id, d.kind, d.options.clone()))
    }

    /// Queues dialog `id`'s answer for the next frame, closing it.
    pub(crate) fn answer_file_dialog(&mut self, id: u64, result: FileDialogResult) {
        if self.file_dialog.as_ref().is_some_and(|d| d.id == id) { self.file_dialog = None; }
        self.queue.push(Action::FileDialog(id, result));
    }

    /// Answers a dialog this host can't show with `why`.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn refuse_file_dialog(&mut self, why: &str) {
        if let Some((id, ..)) = self.take_file_dialog() {
            self.answer_file_dialog(id, Err(FileDialogError(why.into())));
        }
    }

    pub(crate) fn file_dialog_json(&self) -> Option<serde_json::Value> {
        self.file_dialog.as_ref().map(Pending::to_json)
    }
}
