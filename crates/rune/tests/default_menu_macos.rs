//! Main-thread AppKit menu assertions, without opening a window or running an event loop.
#[cfg(target_os = "macos")]
#[path = "../src/native_menu/default_macos.rs"]
mod default_macos;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{sel, rc::Retained};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventModifierFlags as Flags, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSProcessInfo, NSString};
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // SAFETY: This dedicated main-thread process owns every menu. It never opens a
    // window, activates, or dispatches any menu selector.
    unsafe {
        app.setMainMenu(None);
        let root = default_macos::ensure(&app);
        let menu = root.itemAtIndex(0).unwrap().submenu().unwrap();
        let name = NSProcessInfo::processInfo().processName().to_string();
        let titles: Vec<_> = (0..menu.numberOfItems()).map(|i| menu.itemAtIndex(i).unwrap().title().to_string()).collect();
        assert_eq!(titles, [format!("About {name}"), "".into(), "Services".into(), format!("Hide {name}"),
            "Hide Others".into(), "Show All".into(), "".into(), format!("Quit {name}")]);
        let command = Flags::NSEventModifierFlagCommand;
        for (index, selector, key, modifiers) in [
            (0, sel!(orderFrontStandardAboutPanel:), "", command),
            (3, sel!(hide:), "h", command),
            (4, sel!(hideOtherApplications:), "h", command | Flags::NSEventModifierFlagOption),
            (5, sel!(unhideAllApplications:), "", command),
            (7, sel!(terminate:), "q", command),
        ] {
            let item = menu.itemAtIndex(index).unwrap();
            assert_eq!(item.action(), Some(selector));
            assert_eq!(item.keyEquivalent().to_string(), key);
            assert_eq!(item.keyEquivalentModifierMask(), modifiers);
        }
        let services = app.servicesMenu().unwrap();
        assert!(std::ptr::eq(&*services, &*menu.itemAtIndex(2).unwrap().submenu().unwrap()));
        assert_eq!(root.numberOfItems(), 1, "no Edit menu intercepts Rune text commands");
        assert_eq!(app.activationPolicy(), NSApplicationActivationPolicy::Accessory);

        // Repeated promotion preserves the root and any custom items added after it.
        let custom = NSMenuItem::new(mtm);
        custom.setTitle(&NSString::from_str("Custom action"));
        menu.insertItem_atIndex(&custom, 0);
        for _ in 0..2 {
            let current = default_macos::ensure(&app);
            assert!(std::ptr::eq(&*current, &*root));
            assert_eq!(menu.numberOfItems(), 9);
            assert!(std::ptr::eq(&*menu.itemAtIndex(0).unwrap(), &*custom));
        }
        // An app's preexisting menu always wins, including an intentionally empty one.
        let supplied: Retained<NSMenu> = NSMenu::new(mtm);
        app.setMainMenu(Some(&supplied));
        assert!(std::ptr::eq(&*default_macos::ensure(&app), &*supplied));
        assert_eq!(supplied.numberOfItems(), 0);
        let top = NSMenuItem::new(mtm);
        top.setTitle(&NSString::from_str("App-owned menu"));
        supplied.addItem(&top);
        assert!(std::ptr::eq(&*default_macos::ensure(&app), &*supplied));
        assert_eq!(supplied.numberOfItems(), 1);
        assert!(std::ptr::eq(&*app.servicesMenu().unwrap(), &*services));
        app.setMainMenu(None);
    }
    println!("PASS: default titles/selectors/keys, Services, idempotence, custom and empty menus preserved; no activation or Edit menu");
}
