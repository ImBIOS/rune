//! The macOS pasteboard's rich forms. Every form is written after one `clearContents`, so the
//! change count moves once; RTF from apps that offer nothing richer (TextEdit, Notes, Pages)
//! is converted to HTML. Runs on the clipboard worker: AppKit's RTF reader and HTML writer
//! don't need the main thread (its HTML *reader* does, and isn't used).
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::ClassType;
use objc2_app_kit::{
    NSAttributedStringDocumentFormats, NSCharacterEncodingDocumentAttribute, NSDocumentTypeDocumentAttribute, NSHTMLTextDocumentType,
    NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString,
};
use objc2_foundation::{NSAttributedString, NSCopying, NSData, NSDictionary, NSNumber, NSRange, NSString, NSUTF8StringEncoding};
use zeroize::Zeroizing;
use super::ClipboardContent;
use super::html::MAX_RICH_BYTES;

/// Refuse to hand AppKit's RTF parser multi-megabyte blobs (e.g. embedded images).
const MAX_RTF_CONVERT_BYTES: usize = 1024 * 1024;

/// Replaces the pasteboard's contents with plain text and whichever of HTML and RTF `content`
/// carries.
pub(super) fn write(pb: &NSPasteboard, content: &ClipboardContent) -> Result<(), String> {
    autoreleasepool(|_| unsafe {
        pb.clearContents();
        let refused = |form: &str| format!("the pasteboard refused the {form}");
        if !pb.setString_forType(&NSString::from_str(content.text()), NSPasteboardTypeString) { return Err(refused("text")); }
        if let Some(html) = content.html() && !pb.setString_forType(&NSString::from_str(&declare_utf8(html)), NSPasteboardTypeHTML) {
            return Err(refused("HTML"));
        }
        if let Some(rtf) = content.rtf() && !pb.setData_forType(Some(&NSData::with_bytes(rtf.as_bytes())), NSPasteboardTypeRTF) {
            return Err(refused("RTF"));
        }
        Ok(())
    })
}

/// Plain text, HTML and RTF, with HTML made from the RTF when the pasteboard has no HTML.
pub(super) fn read(pb: &NSPasteboard) -> Option<ClipboardContent> {
    autoreleasepool(|_| unsafe {
        let text = pb.stringForType(NSPasteboardTypeString).map(|s| Zeroizing::new(s.to_string()));
        let html = pb.stringForType(NSPasteboardTypeHTML).map(|s| s.to_string()).filter(|h| !h.is_empty() && h.len() <= MAX_RICH_BYTES);
        let rtf = pb.dataForType(NSPasteboardTypeRTF)
            .filter(|d| !d.is_empty() && d.len() <= MAX_RICH_BYTES)
            .map(|d| Zeroizing::new(d.bytes().to_vec()));
        if text.is_none() && html.is_none() && rtf.is_none() { return None; }
        let html = html.or_else(|| rtf.as_deref().and_then(|rtf| rtf_to_html(rtf)));
        let mut content = ClipboardContent::from(text.unwrap_or_default());
        if let Some(html) = html { content = content.with_html(html); }
        if let Some(rtf) = rtf { content = content.with_rtf(String::from_utf8_lossy(&rtf)); }
        Some(content)
    })
}

/// A UTF-8 HTML document for RTF, through `NSAttributedString`.
pub(super) fn rtf_to_html(rtf: &[u8]) -> Option<String> {
    if rtf.is_empty() || rtf.len() > MAX_RTF_CONVERT_BYTES { return None; }
    autoreleasepool(|_| unsafe {
        let string = NSAttributedString::initWithRTF_documentAttributes(NSAttributedString::alloc(), &NSData::with_bytes(rtf), None)?;
        let html_type: Retained<AnyObject> = Retained::cast(NSHTMLTextDocumentType.copy());
        let utf8: Retained<AnyObject> = Retained::cast(NSNumber::new_usize(NSUTF8StringEncoding));
        let attributes = NSDictionary::from_id_slice(&[NSDocumentTypeDocumentAttribute, NSCharacterEncodingDocumentAttribute], &[html_type, utf8]);
        let data = string.dataFromRange_documentAttributes_error(NSRange::new(0, string.length()), &attributes).ok()?;
        if data.len() > MAX_RICH_BYTES { return None; }
        String::from_utf8(data.bytes().to_vec()).ok()
    })
}

/// Browsers read pasteboard HTML without a charset as Latin-1; say it's UTF-8.
fn declare_utf8(html: &str) -> std::borrow::Cow<'_, str> {
    let head = &html.as_bytes()[..html.len().min(1024)];
    if head.windows(7).any(|w| w.eq_ignore_ascii_case(b"charset")) { return html.into(); }
    format!("<meta charset=\"utf-8\">{html}").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTF: &str = r"{\rtf1\ansi\ansicpg1252\cocoartf2761
{\fonttbl\f0\fswiss\fcharset0 Helvetica;}
\pard\f0\fs24 Hello \b bold\b0  and \i italic\i0  caf\'e9}";

    /// Off the main thread, as on the clipboard worker, and bounded so a main-thread wait in
    /// AppKit fails the test instead of hanging it.
    #[test]
    fn rtf_becomes_html_off_the_main_thread() {
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || { let _ = send.send(rtf_to_html(RTF.as_bytes())); });
        let html = recv.recv_timeout(std::time::Duration::from_secs(20)).expect("conversion finished").expect("converted");
        let text: String = super::super::sanitize_html(&html);
        assert!(text.contains("Hello") && text.contains("bold") && text.contains("café"), "{text}");
        assert!(html.contains("font-weight: bold") || html.contains("<b>"), "{html}");
        assert!(rtf_to_html(b"not rtf").is_none());
        assert!(rtf_to_html(&[]).is_none());
        assert!(rtf_to_html(&vec![b'a'; MAX_RTF_CONVERT_BYTES + 1]).is_none(), "oversized RTF is refused before AppKit");
    }

    /// A private pasteboard, never the user's. Skipped where there is no pasteboard server (a
    /// build machine's SSH session).
    #[test]
    fn every_form_round_trips_in_one_change() {
        let pb: Option<Retained<NSPasteboard>> = unsafe { objc2::msg_send_id![NSPasteboard::class(), pasteboardWithUniqueName] };
        let Some(pb) = pb else { return eprintln!("no pasteboard server; skipped") };
        let before = unsafe { pb.changeCount() };
        let content = ClipboardContent::new("Hi there").with_html("<b>Hi</b> there").with_rtf(RTF);
        write(&pb, &content).unwrap();
        assert_eq!(unsafe { pb.changeCount() }, before + 1);
        let back = read(&pb).unwrap();
        assert_eq!(back.text(), "Hi there");
        assert!(back.html().unwrap().ends_with("<b>Hi</b> there"));
        assert_eq!(back.rtf(), Some(RTF));

        unsafe {
            pb.clearContents();
            pb.setData_forType(Some(&NSData::with_bytes(RTF.as_bytes())), NSPasteboardTypeRTF);
        }
        let rtf_only = read(&pb).unwrap();
        assert_eq!(rtf_only.text(), "Hello bold and italic café", "the pasteboard derives text from RTF");
        assert!(rtf_only.html().unwrap().contains("bold"));
        let _: () = unsafe { objc2::msg_send![&pb, releaseGlobally] };
    }
}
