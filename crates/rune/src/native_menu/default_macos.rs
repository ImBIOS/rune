//! The winit 0.30 app menu for an accessory app becoming a regular app. Creating
//! a custom Rune menu uses the same root, so either installation order composes.
use objc2::{rc::Retained, runtime::Sel, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSProcessInfo, NSString};

/// Preserve any existing menu, including an app-supplied empty menu. AppKit owns
/// the fallback for the app's lifetime; removing Rune's custom items leaves it alone.
pub(crate) fn ensure(app: &NSApplication) -> Retained<NSMenu> {
    // SAFETY: NSApplication and its menus are main-thread-only. All objects are
    // retained and the standard selectors use AppKit's responder chain, as in winit.
    unsafe {
        if let Some(root) = app.mainMenu() { return root; }
        let mtm = MainThreadMarker::from(app);
        let root = NSMenu::new(mtm);
        let top = NSMenuItem::new(mtm);
        let menu = NSMenu::new(mtm);
        let name = NSProcessInfo::processInfo().processName().to_string();
        let services = NSMenu::new(mtm);
        let services_item = item(mtm, "Services", None, "");
        services_item.setSubmenu(Some(&services));
        let hide_others = item(mtm, "Hide Others", Some(sel!(hideOtherApplications:)), "h");
        hide_others.setKeyEquivalentModifierMask(NSEventModifierFlags::NSEventModifierFlagOption
            | NSEventModifierFlags::NSEventModifierFlagCommand);
        for entry in [
            item(mtm, &format!("About {name}"), Some(sel!(orderFrontStandardAboutPanel:)), ""),
            NSMenuItem::separatorItem(mtm),
            services_item,
            item(mtm, &format!("Hide {name}"), Some(sel!(hide:)), "h"),
            hide_others,
            item(mtm, "Show All", Some(sel!(unhideAllApplications:)), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, &format!("Quit {name}"), Some(sel!(terminate:)), "q"),
        ] { menu.addItem(&entry); }
        top.setSubmenu(Some(&menu));
        root.addItem(&top);
        app.setServicesMenu(Some(&services));
        app.setMainMenu(Some(&root));
        root
    }
}

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    // SAFETY: Standard NSMenuItem initializer on the main thread; nil target lets
    // AppKit route the action. NSMenuItem defaults to Command for its key equivalent.
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(),
        &NSString::from_str(title), action, &NSString::from_str(key)) }
}
