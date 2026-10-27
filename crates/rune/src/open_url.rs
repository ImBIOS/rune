//! Open a link in the system's default handler: [`Tx::open_url`](crate::Tx::open_url).
//!
//! Only `http:`, `https:` and `mailto:` links open, the same set the web build allows. Anything
//! else (`file:`, custom app schemes, `javascript:`) is refused with
//! [`OpenUrlError::UnsupportedScheme`] before anything runs, as are empty links and links with
//! spaces or control characters. The check runs when the handler calls `open_url`; the open
//! itself happens once the handler's frame is done.
//!
//! macOS asks `NSWorkspace`, Windows `ShellExecuteW`, Linux runs `xdg-open`. Headless runs and
//! tests never open anything: they keep the requests for [`Runtime::take_url_requests`].
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Runtime;

const MAX_LEN: usize = 8192;

/// Why a link was refused or didn't open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenUrlError {
    /// Nothing to open.
    Empty,
    /// Not a link: too long, has whitespace or control characters, or lacks a scheme or host.
    Invalid,
    /// A scheme other than `http`, `https` or `mailto` (lowercased).
    UnsupportedScheme(String),
    /// The system couldn't open it, or this platform has no way to.
    Failed(String),
}

impl std::fmt::Display for OpenUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty link"),
            Self::Invalid => f.write_str("not a valid link"),
            Self::UnsupportedScheme(s) => write!(f, "links with the {s}: scheme can't be opened"),
            Self::Failed(why) => write!(f, "couldn't open the link: {why}"),
        }
    }
}
impl std::error::Error for OpenUrlError {}

/// The link to open, trimmed, or why it can't be.
pub(crate) fn check(url: &str) -> Result<String, OpenUrlError> {
    let url = url.trim();
    if url.is_empty() { return Err(OpenUrlError::Empty); }
    if url.len() > MAX_LEN || url.chars().any(|c| c.is_whitespace() || c.is_control()) { return Err(OpenUrlError::Invalid); }
    let (scheme, rest) = url.split_once(':').ok_or(OpenUrlError::Invalid)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme.is_empty() || !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return Err(OpenUrlError::Invalid);
    }
    match scheme.as_str() {
        "http" | "https" => {
            let host = rest.strip_prefix("//").ok_or(OpenUrlError::Invalid)?;
            if host.starts_with(['/', '?', '#']) || host.is_empty() { return Err(OpenUrlError::Invalid); }
        }
        "mailto" => if rest.is_empty() { return Err(OpenUrlError::Invalid); },
        _ => return Err(OpenUrlError::UnsupportedScheme(scheme)),
    }
    Ok(url.to_owned())
}

/// Set once a real window shell runs; headless runs and tests leave it clear.
static NATIVE: AtomicBool = AtomicBool::new(false);
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn arm() { NATIVE.store(true, Ordering::Relaxed); }

impl<S: 'static> Runtime<S> {
    /// Takes the links handlers asked to open that this runtime didn't open: all of them in a
    /// headless run or test, none in a window. Each was already checked.
    pub fn take_url_requests(&mut self) -> Vec<String> { std::mem::take(&mut self.url_requests) }

    pub(crate) fn open_urls(&mut self, urls: Vec<String>) {
        for url in urls {
            if cfg!(target_arch = "wasm32") || NATIVE.load(Ordering::Relaxed) {
                if let Err(e) = open(&url) { eprintln!("rune: open_url({url}): {e}"); }
            } else {
                self.url_requests.push(url);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn open(url: &str) -> Result<(), OpenUrlError> { crate::web::open_url(url).map_err(OpenUrlError::Failed) }

#[cfg(target_os = "macos")]
fn open(url: &str) -> Result<(), OpenUrlError> {
    use objc2::{class, msg_send, rc::Retained};
    use objc2_foundation::{NSString, NSURL};
    let ns = NSString::from_str(url);
    // SAFETY: NSWorkspace's shared instance lives for the process; `openURL:` takes an NSURL
    // and returns BOOL. Handlers run on the UI thread.
    unsafe {
        let url: Option<Retained<NSURL>> = NSURL::URLWithString(&ns);
        let url = url.ok_or(OpenUrlError::Invalid)?;
        let workspace: *mut objc2::runtime::AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let opened: objc2::runtime::Bool = msg_send![workspace, openURL: &*url];
        if opened.as_bool() { Ok(()) } else { Err(OpenUrlError::Failed("no app opened it".into())) }
    }
}

#[cfg(target_os = "windows")]
fn open(url: &str) -> Result<(), OpenUrlError> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, target) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call; the other pointers are null.
    let result = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    // Values above 32 are success.
    if result as usize > 32 { Ok(()) } else { Err(OpenUrlError::Failed(format!("ShellExecute returned {}", result as usize))) }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
fn open(url: &str) -> Result<(), OpenUrlError> {
    // `--` keeps a link from being read as an option; the check already refuses whitespace.
    let mut child = std::process::Command::new("xdg-open").arg("--").arg(url)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().map_err(|e| OpenUrlError::Failed(e.to_string()))?;
    // Reap it so it doesn't linger as a zombie.
    std::thread::spawn(move || { let _ = child.wait(); });
    Ok(())
}

#[cfg(not(any(target_arch = "wasm32", target_os = "macos", target_os = "windows", all(unix, not(any(target_os = "ios", target_os = "android"))))))]
fn open(_: &str) -> Result<(), OpenUrlError> { Err(OpenUrlError::Failed("this platform can't open links".into())) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cx, Env, Runtime, el::{El, el}};

    #[test]
    fn allows_web_and_mail_links_only() {
        assert_eq!(check("  https://example.com/a?b=1#c ").as_deref(), Ok("https://example.com/a?b=1#c"));
        assert_eq!(check("HTTP://example.com").as_deref(), Ok("HTTP://example.com"));
        assert_eq!(check("mailto:someone@example.com?subject=Hi").as_deref(), Ok("mailto:someone@example.com?subject=Hi"));
        assert_eq!(check("file:///etc/passwd"), Err(OpenUrlError::UnsupportedScheme("file".into())));
        assert_eq!(check("JavaScript:alert(1)"), Err(OpenUrlError::UnsupportedScheme("javascript".into())));
        assert_eq!(check("myapp://do/thing"), Err(OpenUrlError::UnsupportedScheme("myapp".into())));
        assert_eq!(check(""), Err(OpenUrlError::Empty));
        assert_eq!(check("  "), Err(OpenUrlError::Empty));
        for bad in ["example.com", "https:", "https://", "https:///x", "https://a b", "https://a\n.com", "mailto:", "1http://x", "://x"] {
            assert_eq!(check(bad), Err(OpenUrlError::Invalid), "{bad:?}");
        }
    }

    #[test]
    fn headless_runs_record_the_request_and_open_nothing() {
        fn view(_: &(), _: &mut Cx<()>) -> El { el().key("root").size(10.0, 10.0) }
        let mut rt = Runtime::new((), view, (50.0, 50.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let mut results = Vec::new();
        rt.update(|_, tx| {
            results.push(tx.open_url("https://example.com/docs"));
            results.push(tx.open_url("file:///etc/hosts"));
            results.push(tx.open_url("mailto:a@example.com"));
        });
        rt.frame(0.0);
        assert_eq!(results, [Ok(()), Err(OpenUrlError::UnsupportedScheme("file".into())), Ok(())]);
        assert_eq!(rt.take_url_requests(), ["https://example.com/docs", "mailto:a@example.com"]);
        assert!(rt.take_url_requests().is_empty());
    }
}
