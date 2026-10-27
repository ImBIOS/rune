use super::*;

#[test]
fn the_scene_filter_lists_only_the_scenes_it_keeps() {
    let mut rt = runtime_with(base().scenes(|name| name.contains('/')), Some(None), true);
    assert!(find(&rt, "playground-item-0").is_none() && find(&rt, "playground-item-1").is_none());
    assert!(find(&rt, "playground-item-2").is_some(), "Motion/Still stays");
    assert_eq!(selected(&rt), 2, "the list starts at its first listed story");
    let live = rt.playground.as_ref().unwrap();
    assert_eq!(live.find("big"), None, "a hidden scene can't be chosen by name");
    assert_eq!(live.find("Still"), Some(2));
    letter(&mut rt, "k");
    assert_eq!(selected(&rt), 4, "K wraps past the hidden scenes");
}
