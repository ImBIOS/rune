#![cfg(all(feature = "dev", debug_assertions))]
//! Parallel `--tree`/`--snap` runs must not fight over the panel's port.
use std::process::{Command, Output};

#[test]
fn headless_runs_leave_the_panel_closed_unless_asked_and_never_panic_on_a_busy_port() {
    let bind = rune_dev_panel::config().bind;
    // Busy either way: if this bind fails, something else already holds the port.
    let _held = std::net::TcpListener::bind(bind);
    let dir = std::env::temp_dir().join(format!("rune-dev-headless-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |name: &str, panel: bool| -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rune-dev-panel"));
        command.arg("--tree").arg(dir.join(format!("{name}.json"))).env_remove("RUNE_DEV_PANEL");
        if panel { command.env("RUNE_DEV_PANEL", "1"); }
        command.output().unwrap()
    };
    let (a, b, asked) = std::thread::scope(|s| {
        let a = s.spawn(|| run("a", false));
        let b = s.spawn(|| run("b", false));
        let asked = s.spawn(|| run("asked", true));
        (a.join().unwrap(), b.join().unwrap(), asked.join().unwrap())
    });
    for output in [&a, &b, &asked] {
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    for output in [&a, &b] {
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Rune dev"), "{}", String::from_utf8_lossy(&output.stderr));
    }
    let asked = String::from_utf8_lossy(&asked.stderr);
    assert!(asked.contains(&format!("port {} is busy", bind.port())), "{asked}");
    std::fs::remove_dir_all(dir).unwrap();
}
