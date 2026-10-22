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

fn fork_env(age: f32, life: f32) -> (f32, f32, f32) {
    let ss = |a: f32, b: f32, x: f32| {
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let grow = 1.0 - (1.0 - (age / 0.05).min(1.0)).powi(3);
    let lifee = ss(0.0, 0.012, age) * (-age / (life * 0.4)).exp() * (1.0 - ss(0.75 * life, life, age));
    (grow, lifee, 1.0 - age / life)
}

/// Electric tables. Layout at `first`: [N, strands, forks, 0], [flicker k0..k2, 0],
/// strands x N/4 displacement vec4s, strands x N/4 blurred displacement (halo path),
/// (border: strands x N/4 crawl vec4s), forks x 6 vec4s.
pub(crate) fn electric(p: &ElectricParams, shape: Shape, t: f32, seed: u32, reduced: bool, data: &mut Vec<[f32; 4]>) -> [f32; 4] {
    let first = data.len() as f32;
    let freq = p.frequency / 100.0;
    let strands = p.strands.clamp(1, 3) as usize;
    let flick = |k: usize| {
        if reduced || p.flicker <= 0.0 {
            1.0
        } else {
            1.0 - p.flicker * 0.55 * (0.5 + 0.5 * gnoise(t * 11.0, k as f32 * 3.7, 0, seed.wrapping_add(17)))
        }
    };
    let rate = if reduced { 0.0 } else { p.forks };
    match p.style {
        ElectricStyle::Arc { a, b } => {
            let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt().max(1.0);
            let n = ((l / 1.0).ceil() as usize).clamp(32, 1024).next_multiple_of(4);
            let hdr = data.len();
            data.push([n as f32, strands as f32, 0.0, 0.0]);
            data.push([flick(0), flick(1), flick(2), 0.0]);
            let mut buf = vec![0.0f32; n];
            let mut smooth: Vec<Vec<f32>> = Vec::new();
            let step = l / (n - 1) as f32;
            let main = |u: f32| disp(u * l * freq, t * p.speed, 0, seed, 5) * p.amplitude * (PI * u).sin().max(0.0).sqrt();
            for k in 0..strands {
                let sk = seed.wrapping_add(k as u32 * 7919);
                let ka = if k == 0 { 1.0 } else { 0.7 };
                let tk = t * p.speed * (1.0 + 0.35 * k as f32);
                for (i, v) in buf.iter_mut().enumerate() {
                    let u = i as f32 / (n - 1) as f32;
                    *v = disp(u * l * freq, tk, 0, sk, 5) * p.amplitude * ka * (PI * u).sin().max(0.0).sqrt();
                }
                push_samples(data, &buf);
                smooth.push(blur3(&buf, p.glow * 1.2 / step, false));
            }
            // The halo follows the blurred path (a real bloom of the line), so it never striates.
            for sm in &smooth {
                push_samples(data, sm);
            }
            let dir = [(b[0] - a[0]) / l, (b[1] - a[1]) / l];
            let nrm = [-dir[1], dir[0]];
            let mut forks = 0;
            for j in 0..4u32 {
                let Some(f) = fork_slot(seed, j, t, rate, 0.26, 0.2) else { continue };
                let hk = f.hk;
                let u0 = 0.15 + 0.7 * u01(hk + 3);
                let side = if u01(hk + 4) > 0.5 { 1.0 } else { -1.0 };
                let ang = (0.4 + 0.5 * u01(hk + 5)) * side;
                let len = (l * (0.04 + 0.07 * u01(hk + 6))).clamp(10.0, 56.0);
                let d0 = main(u0);
                let o = [a[0] + dir[0] * u0 * l + nrm[0] * d0, a[1] + dir[1] * u0 * l + nrm[1] * d0];
                let (sa, ca) = ang.sin_cos();
                let fd = [dir[0] * ca - dir[1] * sa, dir[0] * sa + dir[1] * ca];
                let (grow, lifee, heat) = fork_env(f.age, f.life);
                data.push([o[0], o[1], fd[0], fd[1]]);
                data.push([len, grow, lifee, heat]);
                let mut s = [0.0f32; FORK_SAMPLES];
                for (i, v) in s.iter_mut().enumerate() {
                    let fu = i as f32 / (FORK_SAMPLES - 1) as f32;
                    *v = disp(fu * len * freq * 3.0, t * p.speed * 1.3, 0, hk, 4) * (p.amplitude * 0.45 + len * 0.12) * fu.sqrt();
                }
                push_samples(data, &s);
                forks += 1;
            }
            data[hdr][2] = forks as f32;
            [first, n as f32, forks as f32, strands as f32]
        }
        ElectricStyle::Border | ElectricStyle::Charged => {
            let calm = p.style == ElectricStyle::Charged;
            let total = rrect_total(shape.rect, shape.radius);
            let n = ((total / 1.0).ceil() as usize).clamp(64, 2048).next_multiple_of(4);
            let cells = ((total * freq).round() as i32).max(3);
            let sp = if calm { p.speed * 0.35 } else { p.speed };
            let oct = if calm { 3 } else { 5 };
            let ns = if calm { 1 } else { strands };
            let hdr = data.len();
            data.push([n as f32, ns as f32, 0.0, 0.0]);
            data.push([flick(0), flick(1), flick(2), 0.0]);
            let mut buf = vec![0.0f32; n];
            let mut smooth: Vec<Vec<f32>> = Vec::new();
            let step = total / n as f32;
            for k in 0..ns {
                let sk = seed.wrapping_add(k as u32 * 7919);
                let ka = if k == 0 { 1.0 } else { 0.75 };
                let tk = t * sp * (1.0 + 0.4 * k as f32);
                for (i, v) in buf.iter_mut().enumerate() {
                    let x = i as f32 / n as f32 * cells as f32;
                    *v = disp(x, tk, cells, sk, oct) * p.amplitude * ka;
                }
                push_samples(data, &buf);
                smooth.push(blur3(&buf, p.glow * 1.2 / step, true));
            }
            for sm in &smooth {
                push_samples(data, sm);
            }
            for k in 0..ns {
                let sk = seed.wrapping_add(k as u32 * 7919);
                let cc = (cells / 3).max(2);
                for (i, v) in buf.iter_mut().enumerate() {
                    let cx = i as f32 / n as f32 * cc as f32;
                    let c = 0.5 + 0.5 * gnoise(cx - t * sp * 0.08 * cc as f32, t * 0.35 + k as f32 * 5.0, cc, sk.wrapping_add(3));
                    let ss = |a: f32, b: f32, x: f32| {
                        let q = ((x - a) / (b - a)).clamp(0.0, 1.0);
                        q * q * (3.0 - 2.0 * q)
                    };
                    *v = if calm { 0.45 + 0.55 * ss(0.2, 0.8, c) } else { 0.2 + 0.8 * ss(0.25, 0.75, c) };
                }
                push_samples(data, &buf);
            }
            let rate = if calm { rate * 0.5 } else { rate };
            let flen = if calm { p.fork_len * 0.4 } else { p.fork_len };
            let mut forks = 0;
            for j in 0..5u32 {
                let Some(f) = fork_slot(seed, j, t, rate, 0.3, 0.25) else { continue };
                let hk = f.hk;
                let s0 = u01(hk + 3) * total;
                let len = flen * (0.6 + 0.6 * u01(hk + 4));
                let (grow, lifee, heat) = fork_env(f.age, f.life);
                data.push([s0, total, 0.0, 0.0]);
                data.push([len, grow, lifee, heat]);
                let mut s = [0.0f32; FORK_SAMPLES];
                for (i, v) in s.iter_mut().enumerate() {
                    let fu = i as f32 / (FORK_SAMPLES - 1) as f32;
                    *v = disp(fu * len * freq * 2.2, t * p.speed * 1.3, 0, hk, 4) * (p.amplitude * 0.8 + len * 0.1) * fu.sqrt();
                }
                push_samples(data, &s);
                forks += 1;
            }
            data[hdr][2] = forks as f32;
            [first, n as f32, forks as f32, ns as f32]
        }
    }
}

/// Embers: two vec4 per live ember: (pos, streak), (size, temperature, 0, 0). Returns (first, count).
pub(crate) fn embers(p: &FireParams, rect: Rect, radius: f32, t: f32, seed: u32, data: &mut Vec<[f32; 4]>) -> (f32, f32) {
    let first = data.len() as f32;
    let mut count = 0;
    let life0 = p.ember_life.max(0.1);
    for k in 0..p.embers.min(40) {
        let hk0 = hu(seed.wrapping_add(991), k * 7 + 3);
        let life = life0 * (0.6 + 0.8 * u01(hk0));
        let tau = t / life + u01(hk0 + 1);
        let kk = tau.floor();
        let a = tau - kk;
        let hk = hu(hk0, kk as i32 as u32);
        let age = a * life;
        let x0 = rect.x + radius * 0.6 + (rect.w - radius * 1.2) * u01(hk + 2);
        let y0 = rect.y + 1.5;
        let vy = p.ember_rise * (0.6 + 0.8 * u01(hk + 3));
        let sway = 3.0 + 6.0 * u01(hk + 4);
        let fq = 2.0 + 3.0 * u01(hk + 5);
        let ph = TAU * u01(hk + 6);
        let drift = (u01(hk + 7) - 0.5) * 18.0;
        let y = y0 - vy * age * (1.0 - 0.3 * a);
        let x = x0 + sway * (age * fq + ph).sin() * a + drift * age;
        let vyy = -vy * (1.0 - 0.6 * a);
        let vxx = sway * ((age * fq + ph).cos() * fq * a + (age * fq + ph).sin() / life) + drift;
        let sz = p.ember_size * (0.55 + 0.9 * u01(hk + 8)) * (1.0 - 0.45 * a);
        let attack = (a / 0.06).clamp(0.0, 1.0);
        let temp = (1.0 - a).powf(1.3) * (0.75 + 0.35 * u01(hk + 9)) * attack * attack * (3.0 - 2.0 * attack);
        if temp < 0.004 {
            continue;
        }
        let k_shutter = 0.8 / 60.0;
        data.push([x, y, vxx * k_shutter, vyy * k_shutter]);
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
