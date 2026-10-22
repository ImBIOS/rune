// One instanced "uber-quad": rounded rects with per-corner radius and border (kind 0), analytic
// Gaussian rounded-rect shadows (kind 1), glyph-atlas text (kind 2), inset shadows (kind 3) and
// vertical-gradient rounded rects (kind 4). Every instance carries a
// soft-edge clip. Output is premultiplied linear colour into an sRGB target (linear blending).

struct Globals {
    viewport: vec2<f32>,
    pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;

struct In {
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,
    @location(1) radii: vec4<f32>,
    @location(2) fill: vec4<f32>,
    @location(3) border: vec4<f32>,
    @location(4) params: vec4<f32>,
    @location(5) uv: vec4<f32>,
    @location(6) clip: vec4<f32>,
    @location(7) fade: vec4<f32>,
};

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) rect: vec4<f32>,
    @location(2) @interpolate(flat) radii: vec4<f32>,
    @location(3) @interpolate(flat) fill: vec4<f32>,
    @location(4) @interpolate(flat) border: vec4<f32>,
    @location(5) @interpolate(flat) params: vec4<f32>,
    @location(6) @interpolate(flat) uv: vec4<f32>,
    @location(7) @interpolate(flat) clip: vec4<f32>,
    @location(8) @interpolate(flat) fade: vec4<f32>,
};

@vertex
fn vs(i: In) -> Out {
    let corner = vec2<f32>(f32(i.vi & 1u), f32(i.vi >> 1u));
    let kind = i.params.y;
    var grow = 1.0 + i.params.z * 2.0;
    if (kind > 0.5 && kind < 1.5) {
        grow = i.params.z * 3.0 + 1.0;
    } else if (kind > 1.5 && kind < 2.5) {
        grow = 0.0;
    } else if (kind > 2.5 && kind < 3.5) {
        grow = 1.0;
    }
    let lo = i.rect.xy - vec2<f32>(grow);
    let size = i.rect.zw + vec2<f32>(2.0 * grow);
    let p = lo + corner * size;
    var o: Out;
    o.pos = vec4<f32>(p.x / g.viewport.x * 2.0 - 1.0, 1.0 - p.y / g.viewport.y * 2.0, 0.0, 1.0);
    o.p = p;
    o.rect = i.rect;
    o.radii = i.radii;
    o.fill = i.fill;
    o.border = i.border;
    o.params = i.params;
    o.uv = i.uv;
    o.clip = i.clip;
    o.fade = i.fade;
    return o;
}

fn sd_rounded(p: vec2<f32>, half: vec2<f32>, radii: vec4<f32>) -> f32 {
    // radii: top-left, top-right, bottom-right, bottom-left (y down).
    var r = radii.x;
    if (p.x > 0.0) {
        r = select(radii.y, radii.z, p.y > 0.0);
    } else {
        r = select(radii.x, radii.w, p.y > 0.0);
    }
    r = min(r, min(half.x, half.y));
    let q = abs(p) - half + vec2<f32>(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - r;
}

fn ramp(d: f32, fade: f32) -> f32 {
    if (fade <= 0.0) {
        return clamp(d + 0.5, 0.0, 1.0);
    }
    let x = clamp(d / fade, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

fn clip_alpha(p: vec2<f32>, c: vec4<f32>, f: vec4<f32>) -> f32 {
    return ramp(p.x - c.x, f.x) * ramp(p.y - c.y, f.y) * ramp(c.z - p.x, f.z) * ramp(c.w - p.y, f.w);
}

// Evan Wallace's analytic rounded-box shadow (as in GPUI): exact along x, 4 samples along y.
fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (sqrt(2.0 * 3.14159265) * sigma);
}

fn erf2(x: vec2<f32>) -> vec2<f32> {
    let s = sign(x);
    let a = abs(x);
    var r = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    r = r * r;
    return s - s / (r * r);
}

fn shadow_x(x: f32, y: f32, sigma: f32, corner: f32, half: vec2<f32>) -> f32 {
    let delta = min(half.y - corner - abs(y), 0.0);
    let curved = half.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    let integral = 0.5 + 0.5 * erf2((x + vec2<f32>(-curved, curved)) * (sqrt(0.5) / sigma));
    return integral.y - integral.x;
}

fn shadow(p: vec2<f32>, half: vec2<f32>, sigma: f32, corner: f32) -> f32 {
    let lo = p.y - half.y;
    let hi = p.y + half.y;
    let start = clamp(-3.0 * sigma, lo, hi);
    let end = clamp(3.0 * sigma, lo, hi);
    let step = (end - start) / 4.0;
    var y = start + step * 0.5;
    var v = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        v = v + shadow_x(p.x, p.y - y, sigma, corner, half) * gaussian(y, sigma) * step;
        y = y + step;
    }
    return v;
}

@fragment
fn fs(i: Out) -> @location(0) vec4<f32> {
    let kind = i.params.y;
    let half = i.rect.zw * 0.5;
    let center = i.rect.xy + half;
    var col = vec4<f32>(0.0);
    if (kind < 0.5) {
        let d = sd_rounded(i.p - center, half, i.radii);
        let soft = max(i.params.z, 0.5);
        let cov = clamp(0.5 - d / (soft * 2.0), 0.0, 1.0);
        let bw = i.params.x;
        if (bw > 0.0) {
            let inner = clamp(0.5 - (d + bw) / (soft * 2.0), 0.0, 1.0);
            col = i.fill * inner + i.border * (cov - inner);
        } else {
            col = i.fill * cov;
        }
    } else if (kind < 1.5) {
        let sigma = max(i.params.z, 0.01);
        let corner = min(i.radii.x, min(half.x, half.y));
        col = i.fill * shadow(i.p - center, half, sigma, corner);
    } else if (kind > 2.5) {
        let d = sd_rounded(i.p - center, half, i.radii);
        let cov = clamp(0.5 - d, 0.0, 1.0);
        if (kind < 3.5) {
            // Inset shadow: the element's shape minus a blurred copy of it shrunk by the spread
            // and moved by the offset.
            let spread = i.params.x;
            let ih = max(half - vec2<f32>(spread), vec2<f32>(0.0));
            let q = i.p - center - i.uv.xy;
            var lit: f32;
            if (i.params.z < 0.3) {
                lit = clamp(0.5 - sd_rounded(q, ih, max(i.radii - vec4<f32>(spread), vec4<f32>(0.0))), 0.0, 1.0);
            } else {
                lit = shadow(q, ih, i.params.z, max(min(i.radii.x - spread, min(ih.x, ih.y)), 0.0));
            }
            col = i.fill * cov * (1.0 - clamp(lit, 0.0, 1.0));
        } else {
            let t = clamp((i.p.y - i.rect.y) / max(i.rect.w, 1.0), 0.0, 1.0);
            col = mix(i.fill, i.uv, t) * cov;
        }
    } else {
        var cov: f32;
        if i.uv.z > 0.0 {
            // Flying text keeps the original atlas footprint while its quad scales uniformly.
            let p = (i.p - i.rect.xy) * i.uv.zw / i.rect.zw - vec2<f32>(0.5);
            let uv = vec2<i32>(i.uv.xy + floor(p));
            let f = fract(p);
            cov = mix(mix(textureLoad(atlas, uv, 0).r, textureLoad(atlas, uv + vec2<i32>(1, 0), 0).r, f.x),
                      mix(textureLoad(atlas, uv + vec2<i32>(0, 1), 0).r, textureLoad(atlas, uv + vec2<i32>(1, 1), 0).r, f.x), f.y);
        } else {
            let texel = vec2<i32>(i.uv.xy + floor(i.p - i.rect.xy));
            cov = textureLoad(atlas, texel, 0).r;
        }
        col = i.fill * pow(cov, i.params.w);
    }
    return col * clip_alpha(i.p, i.clip, i.fade);
}
