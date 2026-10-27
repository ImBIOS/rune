//! Paint records that keep a place in the painter stream for pixels Rune doesn't paint itself:
//! effect slots a compositor fills (`El::effect`).
use crate::render::{Instance, KIND_EFFECT, LayerSpec, NO_CLIP};

/// Pushes the effect slot `(bleed_pt, backdrop)` over `rect` with corner `radii`, clipped by
/// `clip` and the packed `fade`, at `k_px` device pixels per point and paint opacity `o`.
/// A compositor paints whatever it likes at a slot, so a faded slot gets its own layer that
/// applies the opacity, instead of a factor it could forget. Returns the index of that slot.
pub(super) fn push_effect(instances: &mut Vec<Instance>, (bleed, backdrop): (f32, bool),
    (rect, radii, clip, fade): ([f32; 4], [f32; 4], [f32; 4], [u32; 4]), k_px: f32, o: f32) -> Option<usize> {
    let slot = Instance {
        rect,
        radii,
        fill: [0.0, 0.0, 0.0, 1.0],
        border: [0.0; 4],
        params: [if backdrop { 1.0 } else { 0.0 }, KIND_EFFECT, bleed * k_px, k_px],
        uv: [0.0; 4],
        clip,
        fade,
    };
    if o >= 1.0 {
        instances.push(slot);
        return None;
    }
    let mut marker = Instance::begin_layer(rect, clip, LayerSpec { opacity: o, ..Default::default() });
    marker.fade = fade;
    instances.push(marker);
    instances.push(Instance { clip: NO_CLIP, ..slot });
    instances.push(Instance::end_layer());
    Some(instances.len() - 2)
}
