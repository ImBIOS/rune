- **`Tx::open_folder(options, on_answer)` and `FileDialogs::open_folder` pick a folder.**
  An Open dialog that chooses an existing folder: rfd's `pick_folder` on macOS and Windows,
  the portal's `OpenFile` with `directory` on Linux. It waits in `--tree` as a `fileDialog`
  of kind `"folder"` and scripts answer it with `file-dialog` as they do a file. New
  `FileDialogKind::Folder`: an exhaustive `match` on `FileDialogKind` needs its arm.
