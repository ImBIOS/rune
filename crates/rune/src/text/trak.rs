//! The AAT `trak` table: tracking that depends on the point size. Core Text applies it at track 0
//! to fonts that also have a `STAT` table (the macOS system font among them), and so does
//! HarfRust, but Parley hands HarfRust the font size in device pixels as the point size, so on a
//! 2x display 14 pt text was tracked as 28 pt text. [`Trak::spacing`] corrects for that.

use swash::FontRef;

/// A font's horizontal track-0 tracking, in font units, at each point size its table lists.
pub(super) struct Trak {
    sizes: Vec<(f32, f32)>,
    units_per_em: f32,
}

impl Trak {
    /// The table of a font HarfRust tracks: one with both `trak` and `STAT`.
    pub(super) fn of(font: FontRef<'_>) -> Option<Trak> {
        font.table(swash::tag_from_bytes(b"STAT"))?;
        let data = font.table(swash::tag_from_bytes(b"trak"))?;
        let u16_at = |at: usize| Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?));
        let fixed_at = |at: usize| Some(i32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as f32 / 65536.0);
        let units_per_em = font.metrics(&[]).units_per_em as f32;
        let horizontal = u16_at(6)? as usize;
        if horizontal == 0 {
            return Some(Trak { sizes: Vec::new(), units_per_em });
        }
        let (tracks, sizes) = (u16_at(horizontal)? as usize, u16_at(horizontal + 2)? as usize);
        let size_table = u32::from_be_bytes(data.get(horizontal + 4..horizontal + 8)?.try_into().ok()?) as usize;
        let track = |i: usize| -> Option<(f32, Vec<f32>)> {
            let entry = horizontal + 8 + i * 8;
            let values = u16_at(entry + 6)? as usize;
            let values = (0..sizes).map(|s| Some(u16_at(values + 2 * s)? as i16 as f32)).collect::<Option<Vec<_>>>()?;
            Some((fixed_at(entry)?, values))
        };
        let tracks = (0..tracks).map(track).collect::<Option<Vec<_>>>()?;
        if tracks.is_empty() {
            return Some(Trak { sizes: Vec::new(), units_per_em });
        }
        // Track 0, or the interpolation between the tracks around it, as HarfRust picks it.
        let mut i = 0;
        while i + 1 < tracks.len() && tracks[i + 1].0 <= 0.0 {
            i += 1;
        }
        let mut j = tracks.len() - 1;
        while j > 0 && tracks[j - 1].0 >= 0.0 {
            j -= 1;
        }
        let t = if i == j || (tracks[j].0 - tracks[i].0).abs() < f32::EPSILON { 0.0 } else { -tracks[i].0 / (tracks[j].0 - tracks[i].0) };
        let sizes = (0..sizes)
            .map(|s| {
                let (a, b) = (tracks[i].1[s], tracks[j].1[s]);
                Some((fixed_at(size_table + 4 * s)?, a + t * (b - a)))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Trak { sizes, units_per_em })
    }

    /// Tracking at `points`, in font units: interpolated between the listed sizes and held
    /// beyond them.
    fn units(&self, points: f32) -> f32 {
        let Some(i) = self.sizes.iter().position(|&(size, _)| size >= points) else {
            return self.sizes.last().map_or(0.0, |s| s.1);
        };
        if i == 0 {
            return self.sizes[0].1;
        }
        let ((s0, v0), (s1, v1)) = (self.sizes[i - 1], self.sizes[i]);
        if (s1 - s0).abs() < f32::EPSILON {
            return (v0 + v1) / 2.0;
        }
        v0 + (points - s0) / (s1 - s0) * (v1 - v0)
    }

    /// Letter spacing, in device px, that turns the tracking HarfRust applied to a run at `px`
    /// device pixels into Core Text's tracking at its point size, `px / scale`, or removes it
    /// when `on` is false. HarfRust rounds tracking to whole font units; Core Text truncates.
    pub(super) fn spacing(&self, px: f32, scale: f32, on: bool) -> f32 {
        let applied = self.units(px).round();
        let wanted = if on { self.units(px / scale).trunc() } else { 0.0 };
        (wanted - applied) * px / self.units_per_em
    }
}

#[cfg(test)]
mod tests {
    use super::Trak;

    /// Part of the macOS system font's track-0 row.
    fn system() -> Trak {
        let sizes = [(12.0, 0.0), (13.0, -12.0), (14.0, -22.0), (24.0, 6.0), (28.0, 28.0), (32.0, 26.0), (36.0, 21.0)];
        Trak { sizes: sizes.to_vec(), units_per_em: 2048.0 }
    }

    #[test]
    fn spacing_moves_tracking_from_the_pixel_size_to_the_point_size() {
        let t = system();
        // 14 pt at 2x: HarfRust tracked 28 px by +28 units; 14 pt wants -22.
        assert!((t.spacing(28.0, 2.0, true) - (-22.0 - 28.0) * 28.0 / 2048.0).abs() < 1e-5);
        // At 1x, listed sizes need nothing.
        assert_eq!(t.spacing(14.0, 1.0, true), 0.0);
        // Off removes what HarfRust applied.
        assert!((t.spacing(14.0, 1.0, false) - 22.0 * 14.0 / 2048.0).abs() < 1e-5);
        // Between listed sizes Core Text truncates and HarfRust rounds: 33 pt is 24.75 units.
        assert!((t.spacing(33.0, 1.0, true) - (24.0 - 25.0) * 33.0 / 2048.0).abs() < 1e-5);
        // Beyond the table the ends hold.
        assert_eq!(t.units(8.0), 0.0);
        assert_eq!(t.units(500.0), 21.0);
    }
}
