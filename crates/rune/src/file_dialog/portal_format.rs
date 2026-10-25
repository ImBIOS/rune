//! The FileChooser portal's argument and reply formats, kept free of D-Bus so they're tested on
//! every Unix host.
use super::{FileDialogError, FileDialogOptions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// `x11:<hex xid>`, or empty for no parent.
pub(super) fn parent_window(xid: u32) -> String { if xid == 0 { String::new() } else { format!("x11:{xid:x}") } }

pub(super) type Filter = (String, Vec<(u32, String)>);
/// Portal filters are glob (0) or MIME (1) patterns; Rune's filters are extensions.
pub(super) fn filters(options: &FileDialogOptions) -> Vec<Filter> {
    options.filters.iter().map(|(name, extensions)| {
        (name.clone(), extensions.iter().map(|ext| (0, glob(ext))).collect())
    }).collect()
}
fn glob(extension: &str) -> String {
    let ext = extension.trim_start_matches('*').trim_start_matches('.');
    if ext.is_empty() || ext == "*" { "*".into() } else { format!("*.{ext}") }
}

/// The portal takes folders as NUL-terminated bytes.
pub(super) fn path_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// The `Response` signal: 0 is a choice, 1 a cancelled dialog, 2 a dialog that ended another
/// way (for example closed with its parent), which is also no choice.
pub(super) fn outcome(status: u32, uris: &[String]) -> Result<Option<PathBuf>, FileDialogError> {
    match status {
        0 => uris.first().map(|uri| uri_path(uri)).transpose(),
        1 | 2 => Ok(None),
        n => Err(FileDialogError(format!("file dialog portal response {n}"))),
    }
}

/// A local `file://` URI as a path, percent-decoded to raw bytes.
pub(super) fn uri_path(uri: &str) -> Result<PathBuf, FileDialogError> {
    let invalid = || FileDialogError(format!("file dialog returned a non-local URI: {uri}"));
    let rest = uri.strip_prefix("file://").ok_or_else(invalid)?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(at) if &rest[..at] == "localhost" => &rest[at..],
        _ => return Err(invalid()),
    };
    let bytes = path.split(['?', '#']).next().unwrap_or_default().as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn uris(list: &[&str]) -> Vec<String> { list.iter().map(|s| (*s).into()).collect() }
    #[test]
    fn chosen_uris_become_percent_decoded_paths() {
        assert_eq!(outcome(0, &uris(&["file:///home/a/My%20Export%20%E5%BD%95.csv"])).unwrap(),
            Some(PathBuf::from("/home/a/My Export 录.csv")));
        assert_eq!(outcome(0, &uris(&["file://localhost/tmp/a%2fb", "file:///tmp/second"])).unwrap(),
            Some(PathBuf::from("/tmp/a/b")));
        assert_eq!(outcome(0, &[]).unwrap(), None);
        // A malformed or truncated escape stays literal.
        assert_eq!(uri_path("file:///tmp/100%").unwrap(), PathBuf::from("/tmp/100%"));
        assert_eq!(uri_path("file:///tmp/1%2").unwrap(), PathBuf::from("/tmp/1%2"));
        assert_eq!(uri_path("file:///tmp/%zz").unwrap(), PathBuf::from("/tmp/%zz"));
        // Non-UTF-8 names survive as bytes.
        assert_eq!(uri_path("file:///tmp/%FF").unwrap().as_os_str().as_bytes(), b"/tmp/\xff");
        assert!(uri_path("file://server/share/a").is_err());
        assert!(uri_path("https://example.com/a").is_err());
    }
    #[test]
    fn cancelled_or_ended_dialog_is_no_choice() {
        assert_eq!(outcome(1, &uris(&["file:///tmp/a"])).unwrap(), None);
        assert_eq!(outcome(2, &[]).unwrap(), None);
        assert!(outcome(3, &[]).is_err());
    }
    #[test]
    fn filters_folders_and_parents_use_portal_formats() {
        let options = FileDialogOptions::default().filter("Exports", &["csv", ".json", "*.txt"]).filter("All files", &["*"]);
        assert_eq!(filters(&options), vec![
            ("Exports".to_string(), vec![(0, "*.csv".to_string()), (0, "*.json".to_string()), (0, "*.txt".to_string())]),
            ("All files".to_string(), vec![(0, "*".to_string())]),
        ]);
        assert_eq!(path_bytes(Path::new("/home/a/Downloads")), b"/home/a/Downloads\0");
        assert_eq!(parent_window(0), "");
        assert_eq!(parent_window(0x3a0000f), "x11:3a0000f");
    }
}
