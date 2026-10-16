//! End-to-end tests of the `8bf-host` binary with the C test filter.

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use image::{DynamicImage, ImageBuffer, Rgba};
use predicates::prelude::*;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    plugins: PathBuf,
}

impl Fixture {
    fn new() -> Option<Self> {
        common::test_filter()?;
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir(&plugins).unwrap();
        common::install_native(&plugins, "Invert", "InvertMain");
        common::install_native(&plugins, "Broken", "ErrorMain");
        common::install_native(&plugins, "Descriptor", "SuiteMain");
        let input = DynamicImage::ImageRgba8(ImageBuffer::from_fn(8, 6, |x, y| {
            Rgba([x as u8 * 30, y as u8 * 40, 100, 255])
        }));
        input.save(dir.path().join("in.png")).unwrap();
        Some(Self { dir, plugins })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The binary with an empty config, no cache and only our plug-ins.
    fn cmd(&self) -> Command {
        let config = self.path("config.toml");
        if !config.exists() {
            std::fs::write(&config, "plugin_dirs = []\ncache = false\n").unwrap();
        }
        let mut cmd = Command::cargo_bin("8bf-host").unwrap();
        cmd.env_remove("EIGHTBF_HOST_PLUGIN_PATH")
            .env_remove("EIGHTBF_HOST_TOKEN")
            .arg("--config")
            .arg(&config)
            .arg("--plugin-dir")
            .arg(&self.plugins);
        cmd
    }
}

macro_rules! fixture {
    () => {
        match Fixture::new() {
            Some(fixture) => fixture,
            None => {
                eprintln!("skipped: needs a C compiler (gcc or clang)");
                return;
            }
        }
    };
}

fn rgba(path: &Path) -> image::RgbaImage {
    image::open(path).unwrap().to_rgba8()
}

#[test]
fn help_and_version() {
    Command::cargo_bin("8bf-host")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Exit codes"));
    Command::cargo_bin("8bf-host")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn no_arguments_is_a_usage_error() {
    Command::cargo_bin("8bf-host").unwrap().assert().code(2);
}

#[test]
fn list_shows_installed_filters() {
    let fixture = fixture!();
    fixture.cmd().arg("list").assert().success().stdout(
        predicate::str::contains("Tests/Invert").and(predicate::str::contains("in this process")),
    );
    let output = fixture.cmd().args(["list", "--json"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["filters"].as_array().unwrap().len(), 3);
}

#[test]
fn show_describes_a_filter() {
    let fixture = fixture!();
    fixture
        .cmd()
        .args(["show", "invert"])
        .assert()
        .success()
        .stdout(predicate::str::contains("category:   Tests"));
}

#[test]
fn run_inverts_an_image_in_process() {
    let fixture = fixture!();
    let output = fixture.path("out.png");
    fixture
        .cmd()
        .args(["run", "Tests/Invert"])
        .arg(fixture.path("in.png"))
        .arg(&output)
        .assert()
        .success();
    let (before, after) = (rgba(&fixture.path("in.png")), rgba(&output));
    assert_eq!(after.get_pixel(3, 2).0, [255 - 90, 255 - 80, 155, 255]);
    assert_eq!(after.dimensions(), before.dimensions());
}

#[test]
fn run_in_a_worker_process_gives_the_same_result() {
    let fixture = fixture!();
    let (direct, worker) = (fixture.path("direct.png"), fixture.path("worker.png"));
    fixture
        .cmd()
        .args(["run", "Invert"])
        .arg(fixture.path("in.png"))
        .arg(&direct)
        .assert()
        .success();
    fixture
        .cmd()
        .args(["--run-mode", "worker", "run", "Invert"])
        .arg(fixture.path("in.png"))
        .arg(&worker)
        .assert()
        .success();
    assert_eq!(rgba(&direct), rgba(&worker));
}

#[test]
fn stdin_to_stdout() {
    let fixture = fixture!();
    let input = std::fs::read(fixture.path("in.png")).unwrap();
    let output = fixture
        .cmd()
        .args(["run", "Invert", "-", "-", "--format", "png"])
        .write_stdin(input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let image = image::load_from_memory(&output.stdout).unwrap().to_rgba8();
    assert_eq!(image.get_pixel(0, 0).0, [255, 255, 155, 255]);
}

#[test]
fn presets_save_and_replay_descriptors() {
    let fixture = fixture!();
    let preset = fixture.path("preset.json");
    fixture
        .cmd()
        .args(["run", "Descriptor"])
        .arg(fixture.path("in.png"))
        .arg(fixture.path("first.png"))
        .arg("--save-preset")
        .arg(&preset)
        .assert()
        .success();
    let text = std::fs::read_to_string(&preset).unwrap();
    assert!(
        text.contains("8bf-host-preset") && text.contains("Amnt"),
        "{text}"
    );
    // Edit the preset's amount, as a person scripting a batch would.
    let edited = text.replace("\"value\": 7", "\"value\": 42");
    std::fs::write(&preset, edited).unwrap();
    fixture
        .cmd()
        .args(["run", "Descriptor"])
        .arg(fixture.path("in.png"))
        .arg(fixture.path("second.png"))
        .arg("--preset")
        .arg(&preset)
        .assert()
        .success();
    assert_eq!(
        rgba(&fixture.path("first.png")).get_pixel(0, 0).0[..3],
        [7, 7, 7]
    );
    assert_eq!(
        rgba(&fixture.path("second.png")).get_pixel(0, 0).0[..3],
        [42, 42, 42]
    );
}

#[test]
fn repeat_runs_on_the_last_output() {
    let fixture = fixture!();
    let (twice, thrice, worker) = (
        fixture.path("twice.png"),
        fixture.path("thrice.png"),
        fixture.path("worker.png"),
    );
    fixture
        .cmd()
        .args(["run", "Invert", "--repeat", "2"])
        .arg(fixture.path("in.png"))
        .arg(&twice)
        .assert()
        .success();
    fixture
        .cmd()
        .args(["run", "Invert", "--repeat", "3", "--unload-between-runs"])
        .arg(fixture.path("in.png"))
        .arg(&thrice)
        .assert()
        .success();
    fixture
        .cmd()
        .args(["--run-mode", "worker", "run", "Invert", "--repeat", "2"])
        .arg(fixture.path("in.png"))
        .arg(&worker)
        .assert()
        .success();
    let input = rgba(&fixture.path("in.png"));
    assert_eq!(rgba(&twice), input);
    assert_eq!(rgba(&worker), input);
    assert_eq!(
        rgba(&thrice).get_pixel(3, 2).0,
        [255 - 90, 255 - 80, 155, 255]
    );
}

#[test]
fn unloading_between_runs_needs_in_process() {
    let fixture = fixture!();
    fixture
        .cmd()
        .args([
            "--run-mode",
            "worker",
            "run",
            "Invert",
            "--unload-between-runs",
        ])
        .arg(fixture.path("in.png"))
        .arg(fixture.path("out.png"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("in-process"));
}

#[test]
fn json_summary() {
    let fixture = fixture!();
    let output = fixture
        .cmd()
        .args(["run", "Invert", "--json"])
        .arg(fixture.path("in.png"))
        .arg(fixture.path("out.png"))
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["filter"], "Invert");
}

#[test]
fn exit_codes_distinguish_failures() {
    let fixture = fixture!();
    let input = fixture.path("in.png");
    let out = fixture.path("out.png");
    fixture
        .cmd()
        .args(["run", "Broken"])
        .arg(&input)
        .arg(&out)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("failed on purpose"));
    fixture
        .cmd()
        .args(["run", "Nope"])
        .arg(&input)
        .arg(&out)
        .assert()
        .code(3);
    fixture
        .cmd()
        .args(["run", "Invert"])
        .arg(fixture.path("missing.png"))
        .arg(&out)
        .assert()
        .code(5);
    fixture
        .cmd()
        .args(["run", "Invert", "--rect", "1,2"])
        .arg(&input)
        .arg(&out)
        .assert()
        .code(2);
    std::fs::write(fixture.path("bad.toml"), "run_mode = \"sometimes\"").unwrap();
    Command::cargo_bin("8bf-host")
        .unwrap()
        .arg("--config")
        .arg(fixture.path("bad.toml"))
        .arg("list")
        .assert()
        .code(6);
    fixture
        .cmd()
        .args(["--run-mode", "in-process", "run"])
        .arg(&input)
        .arg(&input)
        .arg(&out)
        .assert()
        .code(predicate::in_iter([3, 4]));
}

#[test]
fn mask_limits_the_change() {
    let fixture = fixture!();
    let mask = DynamicImage::ImageLuma8(ImageBuffer::from_fn(8, 6, |x, _| {
        image::Luma([if x < 4 { 255 } else { 0 }])
    }));
    mask.save(fixture.path("mask.png")).unwrap();
    fixture
        .cmd()
        .args(["run", "Invert", "--mask"])
        .arg(fixture.path("mask.png"))
        .arg(fixture.path("in.png"))
        .arg(fixture.path("out.png"))
        .assert()
        .success();
    let (before, after) = (
        rgba(&fixture.path("in.png")),
        rgba(&fixture.path("out.png")),
    );
    assert_ne!(before.get_pixel(1, 1), after.get_pixel(1, 1));
    assert_eq!(before.get_pixel(6, 1), after.get_pixel(6, 1));
}

#[test]
fn doctor_reports() {
    let fixture = fixture!();
    fixture
        .cmd()
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("3 found, 3 can run here"));
}
