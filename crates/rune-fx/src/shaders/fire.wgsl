// Fire / plasma. variant: 0 flames off the top edge (also Heat), 1 flames around the border, 3 embers only.
// Embers are a CPU particle table (cpu.rs::embers): xf = (first, count).
// c0 ramp low (embers/deep), c1 ramp mid, c2 ramp hot
// p0 (height pt, scale pt, speed (scale units/s), intensity)
// p1 (ember count, ember rise pt/s, ember size pt, ember life s)
// p2 (warp, sides (border variant flame height share), base glow, occlusion)
// p3 (swirl, _, _, _)

fn ramp(T: f32, c0: vec3f, c1: vec3f, c2: vec3f) -> vec3f {
    var col = c0 * smoothstep(0.0, 0.3, T);
    col = mix(col, c1, smoothstep(0.25, 0.62, T));
    col = mix(col, c2, smoothstep(0.58, 0.95, T));
    col = mix(col, vec3f(1.0, 0.97, 0.92), smoothstep(0.9, 1.25, T) * 0.7);
    return col;
}
fn fbm2(p0: vec2f, seed: u32) -> f32 {
    return (0.5 * gnoise(p0, seed) + 0.25 * gnoise(ROT * p0 * 2.02, seed + 131u)) / 0.75;
}
fn fbm4(p0: vec2f, seed: u32) -> f32 {
    var p = p0; var a = 0.5; var s = 0.0;
    for (var o = 0u; o < 4u; o++) { s += a * gnoise(p, seed + o * 131u); p = ROT * p * 2.02; a *= 0.5; }
    return s / 0.9375;
}

fn shade(in: VOut) -> vec4f {
    let i = insts[in.id];
    let c = ctx(i, in.px);
    let H = i.p0.x;
    let sc = max(i.p0.y, 1.0);
    let speed = i.p0.z;
    let inten = i.p0.w;
    let t = c.t;
    var emis = vec3f(0.0);
    var alpha = 0.0;

    if (c.variant != 3) {
        let nrm = sd_grad(c);
        let up = clamp(-nrm.y, 0.0, 1.0);
        var hl: f32;
        if (c.variant == 1) {
            let side = mix(0.14, i.p2.y, smoothstep(-0.3, 0.3, -nrm.y + 0.3));
            hl = H * mix(side, 1.0, smoothstep(0.2, 0.95, up));
        } else {
            // Top edge only: fades over the rounded corners and never past the sides.
            let span = smoothstep(c.half.x + 2.0, c.half.x - c.r * 0.9 - 2.0, abs(c.q.x));
            hl = H * smoothstep(0.15, 0.95, up) * span;
        }
        let hgt = max(c.d + H * 0.06, 0.0) / max(hl, 0.5);
        if (hl > 0.5 && hgt < 1.3) {
            // Domain-warped fbm, stretched vertically into tongues, scrolling upward (buoyancy).
            let q = c.p / sc;
            let tr = t * speed;
            var sw = vec2f(0.0);
            if (i.p3.x > 0.0) { sw = i.p3.x * vec2f(sin(q.y * 0.7 + tr * 0.4), cos(q.x * 0.6 - tr * 0.3)); }
            let wq = q * vec2f(0.9, 0.55) + vec2f(0.0, tr * 0.85);
            let warp = vec2f(fbm2(wq, c.seed), fbm2(wq + vec2f(5.2, 1.3), c.seed + 1u));
            let n = fbm4(q * vec2f(1.0, 0.5) + vec2f(0.0, tr) + warp * i.p2.x + sw, c.seed + 2u);
            let n01 = 0.5 + 0.62 * n;
            var dens = n01 * 1.25 - hgt * 1.05 - 0.05;
            dens += pow(max(1.0 - hgt, 0.0), 3.0) * 0.28;
            // Soft cap toward the reach so tongues thin out instead of ending on a line.
            dens -= smoothstep(0.8, 1.3, hgt) * 0.4;
            let T = clamp(dens * 1.45, 0.0, 1.3);
            let col = ramp(T, i.c0.rgb, i.c1.rgb, i.c2.rgb);
            let e = smoothstep(0.0, 0.35, T) * (0.35 + 0.65 * T);
            emis += col * e;
            alpha += e * i.p2.w;
        }
        // Light the fire casts around its base.
        let bg = i.p2.z * exp(-max(c.d, 0.0) / max(hl * 0.22, 1.0)) * smoothstep(-1.0, 1.0, c.d) * smoothstep(0.1, 0.6, hl / max(H, 1e-3));
        emis += i.c1.rgb * bg * 0.35;
    }

    let ef = u32(i.xf.x);
    let en = u32(i.xf.y);
    for (var k = 0u; k < en; k++) {
        let a = data[ef + k * 2u];
        let b = data[ef + k * 2u + 1u];
        let sd = segdist(c.p, a.xy - a.zw, a.xy);
        let T = b.y;
        let core = gauss(sd.x, b.x);
        let halo = gauss(sd.x, b.x * 4.0) * 0.22;
        emis += ramp(T * 1.15, i.c0.rgb, i.c1.rgb, i.c2.rgb) * (core + halo) * T * 1.6;
        alpha += core * T * 0.5;
    }
    return finish(vec4f(emis * inten, min(alpha, 1.0) * inten), in.px);
}
