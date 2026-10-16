//! Windows filters under Wine, end to end (opt-in).
//!
//! Builds the test filter as a real x64 Windows DLL with PiPL resources,
//! then runs it the way Linux users do: through Wine and the Windows build
//! of 8bf-host, over the job protocol. Set these to enable it:
//!
//!   EIGHTBF_HOST_TEST_WINE_WORKER  the Windows build of 8bf-host (8bf-host.exe)
//!   EIGHTBF_HOST_TEST_WINE         wine binary (default: wine on PATH)
//!   EIGHTBF_HOST_TEST_WINEPREFIX   optional prefix to use
//!
//! zig must be on PATH to cross-compile the DLL.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use eightbf_host::config::Config;
use eightbf_host::host::constants as c;
use eightbf_host::host::descriptors::{DescValue, Descriptor};
use eightbf_host::host::discovery::{read_plugin_path, PluginInfo};
use eightbf_host::host::image::PlaneImage;
use eightbf_host::host::pipl::{build_pipl, int_property, pascal, ByteOrder};
use eightbf_host::host::session::{FilterRequest, FilterResult, Hooks, RunStatus};
use eightbf_host::protocol::Action;
use eightbf_host::runner::{self, Environment, Placement, Platform};

const ENTRIES: [(&str, &str); 3] = [
    ("Invert", "InvertMain"),
    ("Suites", "SuiteMain"),
    ("Error", "ErrorMain"),
];

fn on_path(name: &str) -> bool {
    Command::new(name).arg("version").output().is_ok()
}

fn worker() -> Option<PathBuf> {
    std::env::var_os("EIGHTBF_HOST_TEST_WINE_WORKER").map(PathBuf::from)
}

fn wine() -> String {
    std::env::var("EIGHTBF_HOST_TEST_WINE").unwrap_or_else(|_| "wine".into())
}

/// Builds the Windows DLL once and returns its filters by name.
fn windows_filters() -> Option<&'static [PluginInfo]> {
    static BUILT: OnceLock<Option<Vec<PluginInfo>>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            if worker().is_none() || !on_path("zig") {
                return None;
            }
            let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("winfilter");
            std::fs::create_dir_all(&out).ok()?;
            let mut rc = String::new();
            for (index, (name, entry)) in ENTRIES.iter().enumerate() {
                let mut entry_bytes = entry.as_bytes().to_vec();
                entry_bytes.push(0);
                let pipl = build_pipl(
                    &[
                        (
                            c::PIPL_KIND,
                            int_property(c::FILTER_KIND as i32, ByteOrder::Little),
                        ),
                        (c::PIPL_NAME, pascal(name)),
                        (c::PIPL_CATEGORY, pascal("Wine")),
                        (c::PIPL_CODE_WIN64_X86, entry_bytes),
                    ],
                    ByteOrder::Little,
                );
                std::fs::write(out.join(format!("p{index}.bin")), pipl).ok()?;
                rc.push_str(&format!("{} PiPL \"p{index}.bin\"\n", 16000 + index));
            }
            std::fs::write(out.join("res.rc"), rc).ok()?;
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/testfilter.c");
            std::fs::copy(source, out.join("testfilter.c")).ok()?;
            let status = Command::new("zig")
                .args([
                    "cc",
                    "-target",
                    "x86_64-windows-gnu",
                    "-shared",
                    "-O1",
                    "-o",
                    "testfilter.8bf",
                    "testfilter.c",
                    "res.rc",
                ])
                .current_dir(&out)
                .status()
                .ok()?;
            assert!(
                status.success(),
                "zig could not build the Windows test filter"
            );
            Some(read_plugin_path(&out.join("testfilter.8bf")).expect("PE reader"))
        })
        .as_deref()
}

fn config() -> Config {
    let mut config = Config {
        plugin_dirs: Vec::new(),
        cache: false,
        ..Config::default()
    };
    config.workers.win_x64 = worker();
    config.wine.command = wine();
    config.wine.prefix = std::env::var_os("EIGHTBF_HOST_TEST_WINEPREFIX").map(PathBuf::from);
    config
}

fn run(
    name: &str,
    configure: impl FnOnce(&mut FilterRequest),
) -> Option<(PlaneImage, FilterResult)> {
    let filters = windows_filters()?;
    let info = filters
        .iter()
        .find(|info| info.name() == name)
        .expect("filter");
    let env = Environment {
        platform: Platform::Unix,
        host_arch: "native".into(),
        self_exe: PathBuf::from("8bf-host"),
    };
    let plan = runner::plan(info, &config(), &env).expect("plan");
    assert!(matches!(
        plan.placement,
        Placement::Worker { wine: true, .. }
    ));
    let data: Vec<u8> = (0..6 * 5 * 4).map(|i| ((i * 7) % 256) as u8).collect();
    let image = PlaneImage::from_data(6, 5, 4, 8, data).unwrap();
    let mut request = FilterRequest::new(image.clone(), 3, true);
    configure(&mut request);
    let result = runner::run_worker(&plan, info, &request, Hooks::default(), Action::Run, None);
    Some((image, result))
}

macro_rules! skip_unless_enabled {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => {
                eprintln!("skipped: set EIGHTBF_HOST_TEST_WINE_WORKER; needs wine and zig");
                return;
            }
        }
    };
}

#[test]
fn the_pe_reader_finds_every_filter_in_the_dll() {
    let filters = skip_unless_enabled!(windows_filters());
    let names: Vec<String> = filters.iter().map(PluginInfo::name).collect();
    assert_eq!(names, ["Invert", "Suites", "Error"]);
    assert!(filters.iter().all(|info| info.archs == ["win-x64"]));
}

#[test]
fn invert_under_wine() {
    let (image, result) = skip_unless_enabled!(run("Invert", |_| {}));
    assert!(
        result.is_ok(),
        "{}\n{}",
        result.message,
        result.log.join("\n")
    );
    let output = result.image.unwrap();
    let expected: Vec<u8> = image.data[..3]
        .iter()
        .map(|v| 255 - v)
        .chain([image.data[3]])
        .collect();
    assert_eq!(output.data[..4], expected[..]);
    assert_eq!(result.parameters.unwrap()[..4], 0x1234u32.to_le_bytes());
}

#[test]
fn suites_and_descriptor_under_wine() {
    let (_image, result) = skip_unless_enabled!(run("Suites", |request| {
        let mut replay = Descriptor::default();
        replay.put(c::fourcc(b"Amnt"), c::TYPE_INTEGER, DescValue::Integer(42));
        request.descriptor = Some(replay);
    }));
    assert!(
        result.is_ok(),
        "{}\n{}",
        result.message,
        result.log.join("\n")
    );
    assert_eq!(result.image.unwrap().data[..3], [42, 42, 42]);
}

#[test]
fn error_string_under_wine() {
    let (_image, result) = skip_unless_enabled!(run("Error", |_| {}));
    assert_eq!(result.status, RunStatus::Error);
    assert!(
        result.message.contains("failed on purpose"),
        "{}",
        result.message
    );
}
