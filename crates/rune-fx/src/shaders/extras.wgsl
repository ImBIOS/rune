// Extras. variant: 0 shimmer/sheen sweep, 1 sparkles, 2 aurora, 3 grain.
// shimmer: c0 sheen, c1 base (a = 0: none)  p0 (period, band pt, angle rad, intensity)  p1 (use mask, pause, feather, _)
// sparkle: c0 colour  p0 (count, life s, size pt, intensity)  p1 (margin pt, _, _, _)
// aurora:  c0 c1 c2 curtains  p0 (scale pt, speed, intensity, feather pt)
// grain:   p0 (amount, fps (0 = static), feather, _)

// Inner feather: fully inside the shape, so overlays never leak past the node's edge.
fn cover(c: Ctx, feather: f32) -> f32 { return smoothstep(c.aa * 0.5, -max(feather, c.aa), c.d); }

@fragment
fn fs(in: VOut) -> @location(0) vec4f {
    let i = insts[in.id];
    let c = ctx(i, in.px);

    if (c.variant == 0) {
        var m: f32;
        if (i.p1.x > 0.5) {
            m = textureSampleLevel(source, samp, (c.p - i.aux2.xy) / i.aux2.zw, 0.0).a;
        } else {
            m = cover(c, max(i.p1.z, c.aa * 1.5));
        }
        if (m <= 0.0) { return vec4f(0.0); }
        let ta = tan(i.p0.z);
        let span = c.half.x + c.half.y * abs(ta) + i.p0.y * 2.5;
        let x = c.q.x + c.q.y * ta;
        var sheen: f32;
        if (c.reduced) {
            // Calm: the whole surface breathes slightly instead of a travelling band.
            sheen = 0.35 * (0.5 - 0.5 * cos(TAU * c.t / max(i.p0.x, 0.1)));
        } else {
            let ph = fract(c.t / max(i.p0.x, 0.1));
            let travel = clamp(ph / max(1.0 - i.p1.y, 0.05), 0.0, 1.0);
            let e = travel * travel * (3.0 - 2.0 * travel);
            let xc = mix(-span, span, e);
            sheen = gauss(x - xc, i.p0.y) + 0.35 * gauss(x - xc, i.p0.y * 2.6);
        }
        let base = i.c1.rgb * i.c1.a;
        return finish(vec4f(base * m + i.c0.rgb * sheen * i.p0.w * m, i.c1.a * m + sheen * i.p0.w * m * 0.2), in.px);
    }
    if (c.variant == 1) {
        let f0 = u32(i.xf.x);
        let n = u32(i.xf.y);
        var e = 0.0;
        var core = 0.0;
        for (var k = 0u; k < n; k++) {
            let a = data[f0 + k * 2u];
            let b = data[f0 + k * 2u + 1u];
            let S = a.z;
            let env = a.w;
            let rel = c.p - a.xy;
            let l = vec2f(b.x * rel.x - b.y * rel.y, b.y * rel.x + b.x * rel.y);
            let rw = max(S * 0.055, c.aa * 0.6);
            let fall = pow(max(1.0 - abs(l.x) / S, 0.0), 3.0);
            let fally = pow(max(1.0 - abs(l.y) / S, 0.0), 3.0);
            let rays = gauss(l.y, rw) * fall + gauss(l.x, rw) * fally;
            let r = length(l);
            e += env * (rays * 0.85 + gauss(r, S * 0.5) * 0.25);
            core += env * gauss(r, S * 0.12);
        }
        let col = i.c0.rgb * e + mix(i.c0.rgb, vec3f(1.0), 0.75) * core;
        return finish(vec4f(col * i.p0.w, (e * 0.3 + core * 0.6) * i.p0.w), in.px);
    }
    if (c.variant == 2) {
        let m = cover(c, max(i.p0.w, c.aa));
        if (m <= 0.0) { return vec4f(0.0); }
        let sc = max(i.p0.x, 1.0);
        let q = c.p / sc;
        var t = c.t * i.p0.y;
        if (c.reduced) { t = 0.0; }
        let uy = (c.p.y - i.rect.y) / i.rect.w;
        var col = vec3f(0.0);
        let cols = array<vec3f, 3>(i.c0.rgb, i.c1.rgb, i.c2.rgb);
        // Curtain heights and warps across the width are CPU tables (cpu.rs::aurora).
        let f0 = u32(i.xf.x);
        let ns = u32(i.xf.y);
        let fx = clamp((c.p.x - i.rect.x) / i.rect.z, 0.0, 1.0) * f32(ns - 1u);
        let x0 = min(u32(fx), ns - 2u);
        let fr = fx - f32(x0);
        for (var k = 0; k < 3; k++) {
            let fk = f32(k);
            let yb = f0 + u32(k) * 2u * (ns >> 2u);
            let wb = yb + (ns >> 2u);
            let wv = mix(dat1(wb, x0), dat1(wb, x0 + 1u), fr);
            let yk = mix(dat1(yb, x0), dat1(yb, x0 + 1u), fr);
            let band = gauss(uy - yk, 0.13 + 0.05 * fk);
            let rays = 0.65 + 0.35 * gnoise(vec2f(q.x * 5.0 + wv * 3.0, t * 0.5 + fk), c.seed + 9u);
            // Curtains have a bright lower hem and fade upward.
            let hem = smoothstep(yk + 0.16, yk - 0.02, uy) * 0.5 + 0.5;
            col += cols[k] * band * rays * hem;
        }
        let a = lum(col) * 0.4;
        return finish(vec4f(col * i.p0.z * m, a * i.p0.z * m), in.px);
    }
    // Grain.
    let m = cover(c, max(i.p0.z, c.aa));
    if (m <= 0.0) { return vec4f(0.0); }
    var frame = 0u;
    if (i.p0.y > 0.0 && !c.reduced) { frame = u32(floor(c.t * i.p0.y)); }
    let ip = vec2u(in.px);
    let n = u01(ip.x + pcg(ip.y + pcg(frame + c.seed))) + u01(ip.y * 7919u + pcg(ip.x + 17u + frame)) - 1.0;
    let a = i.p0.x * m;
    if (n >= 0.0) { return vec4f(vec3f(n * a), 0.0); }
    return vec4f(0.0, 0.0, 0.0, -n * a);
}
