//! Per-frame CPU tables: anything that is a 1D function of (time, seed) or a particle list is
//! evaluated here once per frame instead of once per pixel (bolt paths, forks, embers, sparkles,
//! aurora curtains). Same integer hashes as common.wgsl, so CPU and GPU noise agree.

use crate::*;
use std::f32::consts::{PI, TAU};

#[inline]
pub(crate) fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277803737);
    (w >> 22) ^ w
}
#[inline]
pub(crate) fn hu(a: u32, b: u32) -> u32 {
    pcg(a ^ pcg(b.wrapping_add(0x9E3779B9)))
}
#[inline]
pub(crate) fn u01(n: u32) -> f32 {
    (pcg(n) >> 8) as f32 * (1.0 / 16777216.0)
}
#[inline]
fn hcell(cx: i32, cy: i32, seed: u32) -> f32 {
    (pcg((cx as u32) ^ pcg((cy as u32) ^ pcg(seed))) >> 8) as f32 * (1.0 / 16777216.0)
}
#[inline]
fn grad(cx: i32, cy: i32, seed: u32) -> (f32, f32) {
    let a = hcell(cx, cy, seed) * TAU;
    (a.cos(), a.sin())
}
#[inline]
fn fade(f: f32) -> f32 {
    f * f * f * (f * (f * 6.0 - 15.0) + 10.0)
}
/// Gradient noise, ~[-1, 1]; `per` > 0 makes it periodic in x.
pub(crate) fn gnoise(x: f32, y: f32, per: i32, seed: u32) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (x0, x1) = if per > 0 {
        let a = ix.rem_euclid(per);
        (a, (a + 1) % per)
    } else {
        (ix, ix + 1)
    };
    let dot = |cx: i32, cy: i32, dx: f32, dy: f32| {
        let g = grad(cx, cy, seed);
        g.0 * dx + g.1 * dy
    };
    let a = dot(x0, iy, fx, fy);
    let b = dot(x1, iy, fx - 1.0, fy);
    let c = dot(x0, iy + 1, fx, fy - 1.0);
    let d = dot(x1, iy + 1, fx - 1.0, fy - 1.0);
    let (ux, uy) = (fade(fx), fade(fy));
    let ab = a + (b - a) * ux;
    let cd = c + (d - c) * ux;
    (ab + (cd - ab) * uy) * 1.41421
}
/// Multi-octave lateral displacement for bolts, ~[-1.6, 1.6].
pub(crate) fn disp(x0: f32, t0: f32, per0: i32, seed: u32, oct: u32) -> f32 {
    let (mut x, mut t, mut a, mut s, mut n, mut per) = (x0, t0, 1.0f32, 0.0f32, 0.0f32, per0);
    for o in 0..oct {
        s += a * gnoise(x, t, per, seed.wrapping_add(o * 101));
        n += a;
        if per > 0 {
            x *= 2.0;
            per *= 2;
        } else {
            x *= 2.07;
        }
        t *= 1.55;
        a *= 0.5;
    }
    s / n * 1.6
}

fn push_samples(data: &mut Vec<[f32; 4]>, v: &[f32]) {
    for c in v.chunks(4) {
        let mut q = [0.0; 4];
        q[..c.len()].copy_from_slice(c);
        data.push(q);
    }
}

/// Approximate Gaussian blur (three box passes) of a sampled path, radius in samples.
/// `wrap`: periodic (border loops) or clamped (arcs).
fn blur3(v: &[f32], radius: f32, wrap: bool) -> Vec<f32> {
    let n = v.len() as i64;
    let r = ((radius * 0.58).round() as i64).clamp(0, n / 2 - 1);
    if r == 0 {
        return v.to_vec();
    }
    let mut a = v.to_vec();
    let mut b = vec![0.0f32; v.len()];
    let at = |a: &[f32], i: i64| -> f32 {
        if wrap { a[i.rem_euclid(n) as usize] } else { a[i.clamp(0, n - 1) as usize] }
    };
    for _ in 0..3 {
        let mut acc: f32 = (-r..=r).map(|k| at(&a, k)).sum();
        let inv = 1.0 / (2 * r + 1) as f32;
        for i in 0..n {
            b[i as usize] = acc * inv;
            acc += at(&a, i + r + 1) - at(&a, i - r);
        }
        std::mem::swap(&mut a, &mut b);
    }
    a
}

pub(crate) fn rrect_total(rect: Rect, r: f32) -> f32 {
    let r = r.clamp(0.0, rect.w.min(rect.h) * 0.5);
    let b = (rect.w * 0.5 - r, rect.h * 0.5 - r);
    4.0 * (b.1 + r * PI * 0.5 + b.0)
}

const FORK_SAMPLES: usize = 16;

fn ss(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Roughly unit-variance gaussian from three uniforms.
fn gauss3(h: u32) -> f32 {
    (u01(h) + u01(h.wrapping_add(1)) + u01(h.wrapping_add(2)) - 1.5) * 2.0
}

/// Strike timing. Strike `k` starts at `(k + jitter) * period`, so strikes hold for 0.65 to 1.35
/// periods. `period` = infinity (speed 0) is a single strike that never re-strikes.
struct Strikes {
    period: f32,
    seed: u32,
}

impl Strikes {
    fn new(speed: f32, seed: u32) -> Strikes {
        let period = if speed > 0.0 { (0.55 / speed).clamp(0.05, 4.0) } else { f32::INFINITY };
        Strikes { period, seed: hu(seed, 0x5157) }
    }
    fn start(&self, k: i32) -> f32 {
        if self.period.is_infinite() {
            return if k <= 0 { 0.0 } else { f32::INFINITY };
        }
        (k as f32 + 0.35 * u01(hu(self.seed, k as u32))) * self.period
    }
    /// (current strike, its age, its duration)
    fn at(&self, t: f32) -> (i32, f32, f32) {
        if self.period.is_infinite() {
            return (0, t.max(0.0), f32::INFINITY);
        }
        let mut k = (t / self.period).floor() as i32;
        if t < self.start(k) {
            k -= 1;
        }
        let s = self.start(k);
        (k, t - s, self.start(k + 1) - s)
    }
    /// Re-strikes reuse the channel's coarse shape for a few strikes, then it wanders off.
    fn channel(&self, k: i32) -> u32 {
        let c = (k as f32 / 3.0 + 0.5 * u01(hu(self.seed, 77))).floor() as i32;
        hu(self.seed ^ 0xC4A2, c as u32)
    }
    fn strike_seed(&self, k: i32) -> u32 {
        hu(self.seed ^ 0x3D1F, k as u32)
    }
}

/// Brightness of the live strike: a flash as the channel ionises, sometimes a return stroke down
/// the same channel, then a settle while it holds.
fn strike_env(age: f32, dur: f32, flicker: f32, h: u32) -> f32 {
    let flash = 1.0 + 0.65 * (-age / 0.022).exp();
    let a1 = if dur.is_finite() { dur * (0.3 + 0.4 * u01(h)) } else { f32::INFINITY };
    let ret = if u01(h.wrapping_add(1)) < 0.6 && age > a1 { 0.7 * flicker * (-(age - a1) / 0.03).exp() } else { 0.0 };
    let settle = 0.72 + 0.28 * (-age / 0.12).exp();
    flash * settle + ret
}

/// Afterimage of a strike `g` seconds after it was replaced.
fn ghost_env(g: f32) -> f32 {
    0.9 * (-g / 0.07).exp()
}

/// Midpoint displacement. `pts` are the coarse points (pinned arc ends, or a loop); every level
/// halves each segment and pushes the new midpoint sideways by `amp(level)` x gaussian. Levels
/// below `memory` follow the channel seed (shared by re-strikes), finer ones the strike's own.
fn mpd(mut pts: Vec<f32>, periodic: bool, levels: u32, amp: impl Fn(u32) -> f32, channel: u32, strike: u32, memory: u32) -> Vec<f32> {
    for lv in 0..levels {
        let m = pts.len();
        let seg = if periodic { m } else { m - 1 };
        let sd = hu(if lv < memory { channel } else { strike }, lv * 7919 + 1);
        let a = amp(lv);
        let mut next = Vec::with_capacity(seg * 2 + 1);
        for i in 0..seg {
            let (p0, p1) = (pts[i], pts[(i + 1) % m]);
            next.push(p0);
            next.push((p0 + p1) * 0.5 + a * gauss3(hu(sd, i as u32 * 3)));
        }
        if !periodic {
            next.push(pts[m - 1]);
        }
        pts = next;
    }
    pts
}

/// Linear resample of control points: keeps the kinks sharp. `limit` soft-clamps the offset (0 flattens it).
fn resample(ctrl: &[f32], out: &mut [f32], periodic: bool, limit: f32) {
    if limit <= 0.0 {
        out.fill(0.0);
        return;
    }
    let (m, n) = (ctrl.len(), out.len());
    for (i, v) in out.iter_mut().enumerate() {
        let x = if periodic { i as f32 / n as f32 * m as f32 } else { i as f32 / (n - 1) as f32 * (m - 1) as f32 };
        let i0 = (x.floor() as usize).min(m - 1);
        let f = x - i0 as f32;
        let i1 = if periodic { (i0 + 1) % m } else { (i0 + 1).min(m - 1) };
        let d = ctrl[i0] + (ctrl[i1] - ctrl[i0]) * f;
        *v = limit * (d / limit).tanh();
    }
}

/// A fork's sideways wander: a clamped random walk, so it kinks like the main bolt.
fn fork_walk(hk: u32, len: f32) -> [f32; FORK_SAMPLES] {
    let mut s = [0.0f32; FORK_SAMPLES];
    let mut acc = 0.0f32;
    for (i, v) in s.iter_mut().enumerate().skip(1) {
        acc = (acc + gauss3(hu(hk, 100 + i as u32)) * len * 0.07).clamp(-len * 0.3, len * 0.3);
        *v = acc;
    }
    s
}

/// Fork envelope: (grow 0..1 as the branch shoots out, brightness, heat 1 -> 0 as it cools).
fn fork_env(age: f32, life: f32) -> (f32, f32, f32) {
    let grow = 1.0 - (1.0 - (age / 0.035).min(1.0)).powi(3);
    let lifee = ss(0.0, 0.01, age) * (-age / (life * 0.4)).exp() * (1.0 - ss(0.75 * life, life, age));
    (grow, lifee, 1.0 - age / life)
}

/// Sideways scale of an arc's main bolt (pt).
fn arc_amp(p: &ElectricParams, l: f32) -> f32 {
    p.amplitude * (l / 100.0).sqrt()
}
fn arc_fork_len(l: f32) -> f32 {
    (l * 0.16).clamp(12.0, 80.0)
}
/// How far an arc's bolt and forks can reach off the A-B line, so bounds never clip them.
pub(crate) fn arc_reach(p: &ElectricParams, l: f32) -> f32 {
    2.0 * arc_amp(p, l) + if p.forks > 0.0 { arc_fork_len(l) * 1.1 } else { 0.0 }
}

/// Electric tables. Layout at `first`: [N, strands, forks, 0], [strand brightness k0..k2, 0],
/// strands x N/4 displacement vec4s, strands x N/4 blurred displacement (halo path),
/// (border: strands x N/4 crawl vec4s), forks x 6 vec4s.
///
/// Arc and Border are lightning: a fractal (midpoint-displacement) bolt that strikes, holds,
/// and re-strikes, mostly down the same channel. Strand 0 is the live strike; strands 1 and 2
/// are the previous strikes' afterimages. Forks branch off at the strike and die away.
pub(crate) fn electric(p: &ElectricParams, shape: Shape, t: f32, seed: u32, reduced: bool, data: &mut Vec<[f32; 4]>) -> [f32; 4] {
    let first = data.len() as f32;
    let freq = p.frequency / 100.0;
    let strands = p.strands.clamp(1, 3) as usize;
    let rate = if reduced { 0.0 } else { p.forks };
    let st = Strikes::new(if reduced { 0.0 } else { p.speed }, seed);
    let (k, age, dur) = if reduced { (0, 1.0, f32::INFINITY) } else { st.at(t) };
    let still = reduced || st.period.is_infinite();
    let noise = |j: usize| if still { 1.0 } else { 1.0 - p.flicker * 0.25 * (0.5 + 0.5 * gnoise(t * 14.0, j as f32 * 3.7, 0, seed.wrapping_add(17))) };
    let bright = [
        if reduced { 1.0 } else { strike_env(age, dur, p.flicker, st.strike_seed(k)) * noise(0) },
        if still { 0.0 } else { ghost_env(age) * noise(1) },
        if still { 0.0 } else { ghost_env(age + st.start(k) - st.start(k - 1)) * noise(2) },
    ];
    let hdr = data.len();
    data.push([0.0; 4]);
    data.push([bright[0], bright[1], bright[2], 0.0]);
    match p.style {
        ElectricStyle::Arc { a, b } => {
            let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt().max(1.0);
            let n = (l.ceil() as usize).clamp(32, 1024).next_multiple_of(4);
            let step = l / (n - 1) as f32;
            let amp = arc_amp(p, l);
            let levels = ((l * freq * 6.0).max(8.0).log2().ceil() as u32).clamp(3, 8);
            // Self-similar roughness: each halving keeps 2^-0.62 of the offset.
            let path = |ks: i32| {
                let ctrl = mpd(vec![0.0, 0.0], false, levels, |lv| amp * 0.65 * 2f32.powf(-0.62 * lv as f32), st.channel(ks), st.strike_seed(ks), 2);
                let mut v = vec![0.0f32; n];
                resample(&ctrl, &mut v, false, amp * 2.0);
                v
            };
            let paths: Vec<Vec<f32>> = (0..strands.max(2) as i32).map(|j| path(k - j)).collect();
            for v in &paths[..strands] {
                push_samples(data, v);
            }
            // The halo follows the blurred path (a real bloom of the line), so it never striates.
            for v in &paths[..strands] {
                push_samples(data, &blur3(v, p.glow * 1.2 / step, false));
            }
            let dir = [(b[0] - a[0]) / l, (b[1] - a[1]) / l];
            let nrm = [-dir[1], dir[0]];
            let mut forks = 0;
            'strikes: for j in 0..2 {
                let ks = k - j;
                let fage = t - st.start(ks);
                for c in 0..3u32 {
                    if forks == 4 {
                        break 'strikes;
                    }
                    let hk = hu(st.strike_seed(ks) ^ 0xF04C, c);
                    let life = 0.07 + 0.13 * u01(hk + 2);
                    if u01(hk) > rate || fage > life || fage < 0.0 {
                        continue;
                    }
                    let path = &paths[j as usize];
                    let u0 = 0.12 + 0.73 * u01(hk + 3);
                    let i0 = ((u0 * (n - 1) as f32) as usize).clamp(2, n - 3);
                    let slope = (path[i0 + 2] - path[i0 - 2]) / (4.0 * step);
                    let side = if u01(hk + 4) > 0.5 { 1.0 } else { -1.0 };
                    // Branches lean on toward the far electrode, off the local direction of the bolt.
                    let ang = slope.atan() + (0.35 + 0.5 * u01(hk + 5)) * side;
                    let len = arc_fork_len(l) * (0.45 + 0.55 * u01(hk + 6));
                    let o = [a[0] + dir[0] * u0 * l + nrm[0] * path[i0], a[1] + dir[1] * u0 * l + nrm[1] * path[i0]];
                    let (sa, ca) = ang.sin_cos();
                    let fd = [dir[0] * ca - dir[1] * sa, dir[0] * sa + dir[1] * ca];
                    let (grow, lifee, heat) = fork_env(fage, life);
                    data.push([o[0], o[1], fd[0], fd[1]]);
                    data.push([len, grow, lifee * bright[0].max(0.6), heat]);
                    push_samples(data, &fork_walk(hk, len));
                    forks += 1;
                }
            }
            data[hdr] = [n as f32, strands as f32, forks as f32, 0.0];
            [first, n as f32, forks as f32, strands as f32]
        }
        ElectricStyle::Border => {
            let total = rrect_total(shape.rect, shape.radius);
            let n = (total.ceil() as usize).clamp(64, 2048).next_multiple_of(4);
            let step = total / n as f32;
            let cells = ((total * freq).round() as usize).max(3);
            let path = |ks: i32| {
                let ch = st.channel(ks);
                let coarse: Vec<f32> = (0..cells).map(|i| p.amplitude * 0.7 * gauss3(hu(ch, i as u32 * 3 + 5))).collect();
                let ctrl = mpd(coarse, true, 3, |lv| p.amplitude * 0.7 * 2f32.powf(-0.62 * (lv + 1) as f32), ch, st.strike_seed(ks), 1);
                let mut v = vec![0.0f32; n];
                resample(&ctrl, &mut v, true, p.amplitude * 1.7);
                v
            };
            let paths: Vec<Vec<f32>> = (0..strands.max(2) as i32).map(|j| path(k - j)).collect();
            for v in &paths[..strands] {
                push_samples(data, v);
            }
            for v in &paths[..strands] {
                push_samples(data, &blur3(v, p.glow * 1.2 / step, true));
            }
            let mut buf = vec![0.0f32; n];
            for j in 0..strands {
                crawl(&mut buf, cells, p.speed, t, seed.wrapping_add(j as u32 * 7919), j as f32, false);
                push_samples(data, &buf);
            }
            let mut forks = 0;
            'strikes: for j in 0..2 {
                let ks = k - j;
                let fage = t - st.start(ks);
                for c in 0..3u32 {
                    if forks == 5 {
                        break 'strikes;
                    }
                    let hk = hu(st.strike_seed(ks) ^ 0xF04C, c);
                    let life = 0.07 + 0.13 * u01(hk + 2);
                    if u01(hk) > rate || fage > life || fage < 0.0 {
                        continue;
                    }
                    let i0 = ((u01(hk + 3) * n as f32) as usize).min(n - 1);
                    let len = p.fork_len * (0.6 + 0.6 * u01(hk + 4));
                    let (grow, lifee, heat) = fork_env(fage, life);
                    data.push([i0 as f32 * step, total, paths[j as usize][i0], 0.0]);
                    data.push([len, grow, lifee * bright[0].max(0.6), heat]);
                    push_samples(data, &fork_walk(hk, len));
                    forks += 1;
                }
            }
            data[hdr] = [n as f32, strands as f32, forks as f32, 0.0];
            [first, n as f32, forks as f32, strands as f32]
        }
        ElectricStyle::Charged => {
            // The calm UI border keeps a continuously crawling hairline (no strikes).
            data[hdr + 1] = [1.0, 0.0, 0.0, 0.0];
            let total = rrect_total(shape.rect, shape.radius);
            let n = (total.ceil() as usize).clamp(64, 2048).next_multiple_of(4);
            let step = total / n as f32;
            let cells = ((total * freq).round() as i32).max(3);
            let sp = p.speed * 0.35;
            let tm = if reduced { 0.0 } else { t };
            let mut buf = vec![0.0f32; n];
            for (i, v) in buf.iter_mut().enumerate() {
                let x = i as f32 / n as f32 * cells as f32;
                *v = disp(x, tm * sp, cells, seed, 3) * p.amplitude;
            }
            push_samples(data, &buf);
            push_samples(data, &blur3(&buf, p.glow * 1.2 / step, true));
            crawl(&mut buf, cells as usize, p.speed, tm, seed, 0.0, true);
            push_samples(data, &buf);
            let mut forks = 0;
            for j in 0..5u32 {
                let Some(f) = fork_slot(seed, j, t, rate * 0.5, 0.3, 0.25) else { continue };
                let hk = f.hk;
                let len = p.fork_len * 0.4 * (0.6 + 0.6 * u01(hk + 4));
                let (grow, lifee, heat) = fork_env(f.age, f.life);
                data.push([u01(hk + 3) * total, total, 0.0, 0.0]);
                data.push([len, grow, lifee, heat]);
                push_samples(data, &fork_walk(hk, len));
                forks += 1;
            }
            data[hdr] = [n as f32, 1.0, forks as f32, 0.0];
            [first, n as f32, forks as f32, 1.0]
        }
    }
}

/// Brightness travelling around a border loop, so the current visibly flows.
fn crawl(buf: &mut [f32], cells: usize, speed: f32, t: f32, seed: u32, k: f32, calm: bool) {
    let n = buf.len();
    let cc = (cells as i32 / 3).max(2);
    let sp = if calm { speed * 0.35 } else { speed };
    for (i, v) in buf.iter_mut().enumerate() {
        let cx = i as f32 / n as f32 * cc as f32;
        let c = 0.5 + 0.5 * gnoise(cx - t * sp * 0.08 * cc as f32, t * 0.35 + k * 5.0, cc, seed.wrapping_add(3));
        *v = if calm { 0.45 + 0.55 * ss(0.2, 0.8, c) } else { 0.3 + 0.7 * ss(0.25, 0.75, c) };
    }
}

struct ForkSlot {
    hk: u32,
    age: f32,
    life: f32,
}

fn fork_slot(seed: u32, j: u32, t: f32, rate: f32, base_p: f32, var_p: f32) -> Option<ForkSlot> {
    let hj = hu(seed, j * 977 + 13);
    let p = base_p + var_p * u01(hj);
    let tau = t / p + u01(hj + 1);
    let kk = tau.floor();
    let age = (tau - kk) * p;
    let hk = hu(hj, kk as i32 as u32);
    if u01(hk) > rate {
        return None;
    }
    let life = 0.15 + 0.1 * u01(hk + 2);
    if age > life {
        return None;
    }
    Some(ForkSlot { hk, age, life })
}

/// Ember drag (1/s): flung out of the flame fast, they slow to a buoyant drift.
pub(crate) const EMBER_DRAG: f32 = 2.4;

/// Embers: two vec4 per live ember: (pos, streak), (size, temperature, 0, 0). Returns (first, count).
/// Each is flung up out of the flame, slows under drag toward a buoyant drift, sways in the
/// turbulence and cools (white -> orange -> red) until it vanishes.
pub(crate) fn embers(p: &FireParams, rect: Rect, radius: f32, t: f32, seed: u32, data: &mut Vec<[f32; 4]>) -> (f32, f32) {
    let first = data.len() as f32;
    let mut count = 0;
    let life0 = p.ember_life.max(0.1);
    let k = EMBER_DRAG;
    for e in 0..p.embers.min(40) {
        let hk0 = hu(seed.wrapping_add(991), e * 7 + 3);
        let life = life0 * (0.6 + 0.8 * u01(hk0));
        let tau = t / life + u01(hk0 + 1);
        let kk = tau.floor();
        let a = tau - kk;
        let hk = hu(hk0, kk as i32 as u32);
        let age = a * life;
        let x0 = rect.x + radius * 0.6 + (rect.w - radius * 1.2) * u01(hk + 2);
        let y0 = rect.y - p.height * (0.1 + 0.45 * u01(hk + 3));
        let v0 = -p.ember_rise * (1.0 + 0.8 * u01(hk + 4));
        let vt = -p.ember_rise * 0.32;
        let ek = (-k * age).exp();
        let y = y0 + vt * age + (v0 - vt) * (1.0 - ek) / k;
        let vy = vt + (v0 - vt) * ek;
        let sway = 2.5 + 5.0 * u01(hk + 5);
        let fq = 3.0 + 4.0 * u01(hk + 6);
        let ph = TAU * u01(hk + 7);
        let drift = (u01(hk + 8) - 0.5) * 14.0;
        let grow = 1.0 - (-age * 3.0).exp();
        let x = x0 + drift * age + sway * (age * fq + ph).sin() * grow;
        let vx = drift + sway * ((age * fq + ph).cos() * fq * grow + (age * fq + ph).sin() * 3.0 * (-age * 3.0).exp());
        let sz = p.ember_size * (0.5 + 0.8 * u01(hk + 9)) * (1.0 - 0.4 * a);
        let attack = (age / 0.05).clamp(0.0, 1.0);
        let temp = (-age / (life * 0.42)).exp() * (1.0 - a * a) * (0.8 + 0.3 * u01(hk + 10)) * attack;
        if temp < 0.004 {
            continue;
        }
        let k_shutter = 0.6 / 60.0;
        data.push([x, y, vx * k_shutter, vy * k_shutter]);
        data.push([sz, temp, 0.0, 0.0]);
        count += 1;
    }
    (first, count as f32)
}

/// Sparkles: two vec4 each: (pos, size, envelope), (cos, sin, 0, 0).
pub(crate) fn sparkles(p: &SparkleParams, rect: Rect, t: f32, seed: u32, reduced: bool, data: &mut Vec<[f32; 4]>) -> (f32, f32) {
    let first = data.len() as f32;
    let mut count = 0;
    for k in 0..p.count.min(32) {
        let h0 = hu(seed.wrapping_add(313), k * 11 + 1);
        let life = p.life * (0.7 + 0.6 * u01(h0));
        let tau = t / life + u01(h0 + 1);
        let kk = tau.floor();
        let a = tau - kk;
        let hk = hu(h0, kk as i32 as u32);
        let m = p.margin;
        let pos = [rect.x - m + (rect.w + 2.0 * m) * u01(hk + 2), rect.y - m + (rect.h + 2.0 * m) * u01(hk + 3)];
        let size = p.size * (0.55 + 0.7 * u01(hk + 4));
        let env = if reduced { 0.5 * u01(hk + 5) } else { (PI * a).sin().powi(3) };
        if env < 0.003 {
            continue;
        }
        let ang = 0.785398 * u01(hk + 6) * 0.3;
        data.push([pos[0], pos[1], size, env]);
        data.push([ang.cos(), ang.sin(), 0.0, 0.0]);
        count += 1;
    }
    (first, count as f32)
}

pub(crate) const AURORA_SAMPLES: usize = 64;

/// Aurora curtains: for k in 0..3, AURORA_SAMPLES heights then AURORA_SAMPLES warps, across the rect width.
pub(crate) fn aurora(p: &AuroraParams, rect: Rect, t: f32, seed: u32, data: &mut Vec<[f32; 4]>) -> f32 {
    let first = data.len() as f32;
    let tt = t * p.speed;
    let mut ys = [0.0f32; AURORA_SAMPLES];
    let mut ws = [0.0f32; AURORA_SAMPLES];
    for k in 0..3u32 {
        let fk = k as f32;
        for i in 0..AURORA_SAMPLES {
            let x = rect.x + rect.w * i as f32 / (AURORA_SAMPLES - 1) as f32;
            let qx = x / p.scale.max(1.0);
            let mut s = 0.0;
            let mut a = 0.5;
            let (mut px, mut py) = (qx * 0.55 + tt * (0.25 + 0.12 * fk), fk * 3.1 + tt * 0.07);
            for o in 0..3u32 {
                s += a * gnoise(px, py, 0, seed.wrapping_add(k).wrapping_add(o * 131));
                let (nx, ny) = (0.8 * px - 0.6 * py, 0.6 * px + 0.8 * py);
                px = nx * 2.02;
                py = ny * 2.02;
                a *= 0.5;
            }
            let wv = s / 0.875;
            ws[i] = wv;
            ys[i] = 0.3 + 0.2 * fk + 0.22 * wv;
        }
        push_samples(data, &ys);
        push_samples(data, &ws);
    }
    first
}
