//! Which tracks moved in the last tick, and which are set to move.
use super::{AnimationStore, TrackId};
use crate::props::{NodeId, PropId};

impl AnimationStore {
    /// Tracks sampled (or `set`) in the last tick: what changed on screen.
    pub fn touched(&self) -> impl Iterator<Item = (NodeId, PropId)> + '_ {
        self.touched.iter().map(|&t| {
            let m = &self.t_meta[t as usize];
            (m.node, m.prop)
        })
    }

    /// Tracks animating now, ticked yet or not: what's set to move.
    pub fn active(&self) -> impl Iterator<Item = (NodeId, PropId)> + '_ {
        self.active.iter().map(|&t| {
            let m = &self.t_meta[t as usize];
            (m.node, m.prop)
        })
    }

    /// Like [`touched`](Self::touched), with track ids.
    pub fn touched_tracks(&self) -> impl Iterator<Item = (TrackId, NodeId, PropId)> + '_ {
        self.touched.iter().map(|&t| {
            let m = &self.t_meta[t as usize];
            (TrackId { index: t, generation: m.generation }, m.node, m.prop)
        })
    }
}
