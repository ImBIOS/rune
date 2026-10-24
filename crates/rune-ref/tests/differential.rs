#![cfg(feature = "gpu")]
use proptest::{
    prelude::*,
    test_runner::{Config, FileFailurePersistence, TestError, TestRunner},
};
use rune_ref::{
    Frame, Primitive, Scene, compare, generate,
    gpu::{GpuRenderer, text_run},
};
use std::{cell::RefCell, path::PathBuf};

// A provisional 2-LSB cap per primitive; measured maxima and backend in research/24.
// Composite has its own 2-LSB cap, never multiplied by primitive count.
const BUDGET: [u8; 5] = [2, 2, 2, 2, 2];
fn check(gpu: &mut GpuRenderer, scene: &Scene, tolerance: u8) -> Result<u8, String> {
    let mut cpu = Frame::new(scene.width, scene.height, true);
    cpu.render(scene);
    let actual = gpu.render(scene);
    let diff = compare(scene, &cpu, &actual, tolerance);
    if diff.outlier_pixels > 0 {
        Err(serde_json::to_string(&diff).unwrap())
    } else {
        Ok(diff.max_channel)
    }
}
fn save(name: &str, scene: &Scene) {
    let dir = std::env::var_os("RUNE_REF_FAILURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("failures"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(scene).unwrap()).unwrap();
    eprintln!("saved replay {}", path.display());
}

#[test]
fn cpu_gpu_corpus() {
    let mut gpu = GpuRenderer::default();
    let mut maxima = [0u8; 6];
    for seed in 0..128 {
        let scene = generate::seeded(seed, 8);
        for primitive in &scene.primitives {
            let mut single = scene.clone();
            single.primitives = vec![primitive.clone()];
            let kind = primitive.params[1] as usize;
            match check(&mut gpu, &single, BUDGET[kind]) {
                Ok(max) => maxima[kind] = maxima[kind].max(max),
                Err(e) => {
                    save(&format!("seed-{seed}-kind-{kind}"), &single);
                    panic!("{e}");
                }
            }
        }
        match check(&mut gpu, &scene, 2) {
            Ok(max) => maxima[5] = maxima[5].max(max),
            Err(e) => {
                save(&format!("seed-{seed}-composite"), &scene);
                panic!("{e}");
            }
        }
    }
    for seed in 0..16 {
        let mut scene = generate::seeded(seed, 4);
        text_run(&mut scene, seed);
        if let Err(e) = check(&mut gpu, &scene, 2) {
            save(&format!("text-{seed}"), &scene);
            panic!("{e}");
        }
    }
    eprintln!("METRIC channel_max_rect_shadow_glyph_inset_gradient_composite={maxima:?}");
}

#[test]
fn cpu_gpu_proptest() {
    let gpu = RefCell::new(GpuRenderer::default());
    let mut runner = TestRunner::new(Config {
        cases: 128,
        max_shrink_iters: 2048,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/differential.proptest-regressions",
        ))),
        ..Config::default()
    });
    let strategy = prop::collection::vec(any::<[u8; 32]>(), 1..9);
    let result = runner.run(&strategy, |records| {
        let scene = generate::scene(&records);
        check(&mut gpu.borrow_mut(), &scene, 2).map_err(TestCaseError::fail)?;
        Ok(())
    });
    if let Err(TestError::Fail(reason, records)) = &result {
        save("minimized", &generate::scene(records));
        eprintln!("minimized differential failure: {reason}");
    }
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn saved_regressions() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("regressions");
    let mut gpu = GpuRenderer::default();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let scene: Scene = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        scene.validate().unwrap();
        assert!(check(&mut gpu, &scene, 2).is_ok(), "{}", path.display());
    }
}

// Quarter turns about a centre on the pixel grid keep every turned edge half a pixel from the
// nearest pixel centre and every sample on a texel centre, so rasterizer and oracle agree.
// The clipped pass cuts the turned footprint unevenly, so a clip in the glyph's own frame differs.
#[test]
fn rotated_glyphs() {
    use std::f32::consts::{FRAC_PI_2, PI};
    let mut gpu = GpuRenderer::default();
    let clips = [
        (rune_ref::NO_CLIP, [0.0; 4]),
        ([24.5, 15.25, 40.0, 26.75], [3.0, 0.0, 0.0, 2.5]),
    ];
    for (n, angle) in [FRAC_PI_2, PI, PI + FRAC_PI_2, -FRAC_PI_2].into_iter().enumerate() {
        for uv in [[1.0, 1.0, 0.0, 0.0], [1.0, 1.0, 8.0, 5.0]] {
            for (c, (clip, fade)) in clips.into_iter().enumerate() {
                let mut scene = generate::scene(&[]);
                scene.primitives.push(Primitive {
                    node: 1,
                    source: format!("rotated/{n}"),
                    rect: [20.0, 17.0, 16.0, 10.0],
                    radii: [0.0; 4],
                    fill: [0.7, 0.6, 0.5, 0.8],
                    border: [0.0; 4],
                    params: [angle, 2.0, 0.0, 1.0],
                    uv,
                    clip,
                    fade,
                    fade_origin: None,
                });
                if let Err(e) = check(&mut gpu, &scene, 2) {
                    save(&format!("rotated-{n}-{}-clip-{c}", uv[2]), &scene);
                    panic!("{e}");
                }
            }
        }
    }
}

#[test]
fn gpu_metamorphic() {
    let mut gpu = GpuRenderer::default();
    let close = |a: &[u8], b: &[u8]| {
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().zip(b).all(|(a, b)| a.abs_diff(*b) <= 2),
            "metamorphic pixel mismatch"
        );
    };
    for seed in 0..32 {
        let scene = generate::seeded(seed, 8);
        let original = gpu.render(&scene);
        let mut shifted = scene.clone();
        shifted.width += 4;
        shifted.height += 6;
        generate::translate(&mut shifted, 4.0, 6.0);
        let actual = gpu.render(&shifted);
        for y in 0..64 {
            close(
                &original[y * 256..(y + 1) * 256],
                &actual[((y + 6) * 68 + 4) * 4..((y + 6) * 68 + 68) * 4],
            );
        }
        let mut transparent = scene.clone();
        for mut p in scene.primitives.clone() {
            p.fill = [0.0; 4];
            p.border = [0.0; 4];
            if p.params[1] == 4.0 {
                p.uv = [0.0; 4];
            }
            transparent.primitives.push(p);
        }
        assert_eq!(original, gpu.render(&transparent));
        let mut full = scene.clone();
        for p in &mut full.primitives {
            p.clip = rune_ref::NO_CLIP;
            p.fade = [0.0; 4];
        }
        let unclipped = gpu.render(&full);
        for p in &mut full.primitives {
            p.clip = [0.0, 0.0, 64.0, 64.0];
        }
        close(&unclipped, &gpu.render(&full));
        let mut disjoint = scene.clone();
        for p in &mut disjoint.primitives {
            p.clip = [0.0, 0.0, 31.0, 64.0];
            p.fade = [0.0; 4];
        }
        let n = disjoint.primitives.len();
        for mut p in scene.primitives.clone() {
            p.rect[0] += 32.0;
            p.clip = [32.0, 0.0, 64.0, 64.0];
            p.fade = [0.0; 4];
            disjoint.primitives.push(p);
        }
        let first = gpu.render(&disjoint);
        disjoint.primitives.rotate_left(n);
        assert_eq!(first, gpu.render(&disjoint));
    }
}

#[test]
fn cpu_gpu_dashed() {
    let mut gpu = GpuRenderer::default();
    let mut max = 0;
    for seed in 0..64 {
        let scene = generate::dashed(seed, 8);
        for (n, primitive) in scene.primitives.iter().enumerate() {
            let mut single = scene.clone();
            single.primitives = vec![primitive.clone()];
            match check(&mut gpu, &single, 2) {
                Ok(m) => max = max.max(m),
                Err(e) => {
                    save(&format!("dashed-{seed}-{n}"), &single);
                    panic!("{e}");
                }
            }
        }
        if let Err(e) = check(&mut gpu, &scene, 2) {
            save(&format!("dashed-{seed}-composite"), &scene);
            panic!("{e}");
        }
    }
    eprintln!("METRIC channel_max_dashed={max}");
}
