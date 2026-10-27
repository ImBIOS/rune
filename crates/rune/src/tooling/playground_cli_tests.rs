//! `--playground NAME` with `--variant` and `--crop KEY`, headless and the same every run.

use super::*;

#[derive(Default)]
struct Pad {
    x: f32,
    size: f32,
    green: bool,
}

fn pad(p: &Pad, _: &mut Cx<Pad>) -> crate::El {
    let fill = if p.green { crate::oklch(0.7, 0.2, 145.0) } else { crate::oklch(0.6, 0.2, 30.0) };
    crate::el().w_full().h_full().child(crate::el().key("puck").absolute(p.x, 10.0).size(p.size, p.size).fill(fill))
}

static SCENES: &[(&str, fn(&mut Pad))] = &[("Motion/Slide", |p| p.size = 20.0)];

fn app() -> crate::App<Pad> {
    let playground = crate::Playground::<Pad>::new()
        .enabled(true)
        .trigger("Slide", 'r', "Slide", |_, play| play.after(0.3, |p, _| p.x = 60.0))
        .trigger("Slide", 'g', "Green", |p, _| p.green = true)
        .variant("Slide", "Small", |p| p.size = 20.0)
        .variant("Slide", "Big", |p| p.size = 40.0);
    let mut spec = AppSpec::new("pad", (960.0, 240.0), Pad::default, pad);
    spec.scenes = SCENES;
    crate::App::new(spec).playground(playground)
}

fn run(dir: &std::path::Path, name: &str, args: &[&str]) -> std::path::PathBuf {
    let out = dir.join(name);
    std::fs::create_dir_all(&out).unwrap();
    let snap = out.join("snap.png");
    let mut argv: Vec<String> = ["--playground", "Motion/Slide", "--scale", "2", "--fps", "30", "--snap", snap.to_str().unwrap()].map(String::from).into();
    argv.extend(args.iter().map(|a| a.replace("{out}", out.to_str().unwrap())));
    headless(app(), Args::parse_from(argv));
    out
}

fn bytes(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files.into_iter().flat_map(|p| if p.is_dir() { bytes(&p) } else { vec![(p.file_name().unwrap().to_string_lossy().into(), std::fs::read(&p).unwrap())] }).collect()
}

fn png_size(path: &std::path::Path) -> (u32, u32) {
    let b = std::fs::read(path).unwrap();
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    (be(16), be(20))
}

#[test]
fn variant_and_crop_pick_a_storys_variant_and_crop_the_snap_to_an_element() {
    let _turn = HEADLESS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("rune-playground-crop-{}", std::process::id()));
    let parsed = Args::parse_from(["--playground", "--snap", "s.png", "--crop", "1,2,3,4", "--crop", "puck", "--variant", "Big"].map(String::from).into());
    assert_eq!((parsed.crops, parsed.crop_key.as_deref(), parsed.variant.as_deref()), (vec![[1.0, 2.0, 3.0, 4.0]], Some("puck"), Some("Big")));
    let small = run(&dir, "small", &["--variant", "1", "--crop", "puck"]);
    let big = run(&dir, "big", &["--variant", "Big", "--crop", "puck"]);
    let again = run(&dir, "again", &["--variant", "big", "--crop", "puck"]);
    assert_eq!(png_size(&small.join("snap.png")), (40, 40), "the puck's 20 pt at 2x");
    assert_eq!(png_size(&big.join("snap.png")), (80, 80), "--variant, by name");
    assert!(bytes(&big) == bytes(&again), "the same every run");
    std::fs::remove_dir_all(dir).unwrap();
}
