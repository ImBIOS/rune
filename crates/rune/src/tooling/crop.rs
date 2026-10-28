//! `--crop KEY`: a `--snap` cropped to one element's rect.

use super::*;

/// The pixel rect `[x, y, w, h]` of the element keyed or pathed `key` (one in the playground's
/// stage first), rounded out to whole pixels and clipped to the `w` by `h` frame.
pub(crate) fn element_rect<S: 'static>(rt: &Runtime<S>, key: &str, scale: f32, (w, h): (u32, u32)) -> [u32; 4] {
    let all = rt.elements();
    let named = |i: &usize| !all[*i].exiting && (all[*i].key.as_deref() == Some(key) || all[*i].path == key);
    let found = crate::playground::stage_element(&all, key).or_else(|| (0..all.len()).find(named));
    let Some(i) = found else { die(&format!("--crop {key}: no element has that key or path")) };
    let [x, y, rw, rh] = all[i].rect;
    let px = |v: f32, max: u32, round: fn(f32) -> f32| (round(v * scale).max(0.0) as u32).min(max);
    let (x0, y0, x1, y1) = (px(x, w, f32::floor), px(y, h, f32::floor), px(x + rw, w, f32::ceil), px(y + rh, h, f32::ceil));
    if x1 <= x0 || y1 <= y0 { die(&format!("--crop {key}: the element is empty or outside the frame")); }
    [x0, y0, x1 - x0, y1 - y0]
}

/// The `rect` of an RGBA frame `w` pixels wide.
pub(crate) fn crop(px: &[u8], w: u32, [x, y, cw, ch]: [u32; 4]) -> Vec<u8> {
    let row = |r: u32| {
        let start = ((y + r) * w + x) as usize * 4;
        &px[start..start + cw as usize * 4]
    };
    (0..ch).flat_map(row).copied().collect()
}
