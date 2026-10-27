use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::Inbox;
use crate::el::el;
use crate::runtime::{Cx, Env, Runtime};
use crate::{App, AppSpec};

type Links = Vec<String>;

fn app() -> App<Links> {
    let spec = AppSpec::new("links", (100.0, 100.0), Links::new, |_: &Links, _: &mut Cx<Links>| el());
    App::new(spec).on_open_url(|links, url, _| links.push(url.to_owned()))
}

/// What the window host does once its runtime and first window exist: run the queued updates.
fn deliver(app: &App<Links>, rt: &mut Runtime<Links>) -> usize {
    let updates = app.shared.take().updates;
    let n = updates.len();
    for f in updates { rt.update_if(f); }
    n
}

fn runtime() -> Runtime<Links> {
    Runtime::new(Links::new(), |_: &Links, _: &mut Cx<Links>| el(), (100.0, 100.0), Env::default())
}

#[test]
fn without_a_hook_no_listener_is_installed() {
    let inbox = Inbox::new();
    let mut installed = false;
    let spec = AppSpec::new("plain", (100.0, 100.0), || 0, |_: &u32, _: &mut Cx<u32>| el());
    assert!(!inbox.listen(&App::new(spec), || { installed = true; true }));
    assert!(!installed);
    inbox.receive("app://dropped".into());
}

#[test]
fn links_before_the_first_window_arrive_once_in_order_after_it() {
    let (inbox, app) = (Inbox::new(), app());
    let mut installs = 0;
    assert!(inbox.listen(&app, || { installs += 1; true }));
    inbox.receive("app://one".into());
    inbox.receive("app://two?x=1".into());
    let mut rt = runtime();
    assert!(rt.state.is_empty(), "nothing runs before the host drains");
    assert_eq!(deliver(&app, &mut rt), 2);
    assert_eq!(rt.state, ["app://one", "app://two?x=1"]);
    assert!(rt.needs_commit(), "the transaction commits and redraws");
    assert_eq!(deliver(&app, &mut rt), 0, "delivered once");
    assert!(!inbox.listen(&app, || { installs += 1; true }), "one listener per inbox");
    assert_eq!(installs, 1);
}

#[test]
fn links_while_running_wake_the_loop_and_arrive() {
    let (inbox, app) = (Inbox::new(), app());
    assert!(inbox.listen(&app, || true));
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    app.shared.connect(Arc::new(move || { w.fetch_add(1, Ordering::SeqCst); }));
    let mut rt = runtime();
    deliver(&app, &mut rt);
    let before = wakes.load(Ordering::SeqCst);
    inbox.receive("app://later".into());
    assert_eq!(wakes.load(Ordering::SeqCst), before + 1);
    assert_eq!(deliver(&app, &mut rt), 1);
    assert_eq!(rt.state, ["app://later"]);
}

#[test]
fn the_first_app_to_listen_keeps_the_links() {
    let (inbox, first, second) = (Inbox::new(), app(), app());
    assert!(inbox.listen(&first, || true));
    assert!(!inbox.listen(&second, || true));
    inbox.receive("app://x".into());
    assert_eq!(first.shared.take().updates.len(), 1);
    assert!(second.shared.take().updates.is_empty());
}

#[test]
fn a_listener_that_cant_install_leaves_the_inbox_free() {
    let (inbox, app) = (Inbox::new(), app());
    assert!(!inbox.listen(&app, || false), "off the main thread, or another platform");
    assert!(inbox.listen(&app, || true));
}

#[cfg(target_os = "macos")]
#[test]
fn the_url_is_the_events_direct_object() {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;
    let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
    // SAFETY: Foundation constructors; the event is built, never sent.
    unsafe {
        let event: *mut AnyObject = msg_send![class!(NSAppleEventDescriptor), appleEventWithEventClass: code(b"GURL"),
            eventID: code(b"GURL"), targetDescriptor: std::ptr::null_mut::<AnyObject>(), returnID: -1i16, transactionID: 0i32];
        let event = event.as_ref().unwrap();
        assert_eq!(super::mac::url_of(event), None, "no direct object");
        let url = NSString::from_str("myapp://open?id=1");
        let param: *mut AnyObject = msg_send![class!(NSAppleEventDescriptor), descriptorWithString: &*url];
        let _: () = msg_send![event, setParamDescriptor: param, forKeyword: code(b"----")];
        assert_eq!(super::mac::url_of(event).as_deref(), Some("myapp://open?id=1"));
    }
}

