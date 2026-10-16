//! Shared test support: building the C test filter and describing it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use eightbf_host::host::constants as c;
use eightbf_host::host::discovery::{PluginFormat, PluginInfo};
use eightbf_host::host::image::PlaneImage;
use eightbf_host::host::pipl::{build_pipl, int_property, pascal, ByteOrder, FilterCaseInfo, PiPL};

pub const ALL_MODES: u16 = 0xFFFF;
pub const RGB8_ONLY: u16 = 0x8000 >> c::MODE_RGB;

fn compiler() -> Option<String> {
    let candidates = [
        std::env::var("CC").ok(),
        Some("cc".into()),
        Some("gcc".into()),
        Some("clang".into()),
    ];
    candidates.into_iter().flatten().find(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

/// Builds `tests/fixtures/testfilter.c` once, in the layout this build of
/// the host uses (`-DPACK4` with the `pack4` feature, and always on
/// Windows). Returns None when there is no C compiler.
pub fn test_filter() -> Option<&'static Path> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let compiler = compiler()?;
            let suffix = if cfg!(windows) {
                "dll"
            } else if cfg!(target_os = "macos") {
                "dylib"
            } else {
                "so"
            };
            let pack = eightbf_host::host::structs::uses_pack4();
            let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
                "testfilter-{}.{suffix}",
                if pack { "pack4" } else { "natural" }
            ));
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/testfilter.c");
            let mut command = Command::new(compiler);
            command.args(["-shared", "-O1", "-Wall", "-Wextra", "-Werror"]);
            if !cfg!(windows) {
                command.arg("-fPIC");
            }
            if pack {
                command.arg("-DPACK4");
            }
            let status = command.arg("-o").arg(&out).arg(&source).status().ok()?;
            assert!(status.success(), "compiling the test filter failed");
            Some(out)
        })
        .as_deref()
}

/// Skips the calling test (by returning) when the filter cannot be built.
#[macro_export]
macro_rules! require_filter {
    () => {
        match common::test_filter() {
            Some(path) => path,
            None => {
                eprintln!("skipped: needs a C compiler (gcc or clang)");
                return;
            }
        }
    };
}

pub fn make_info(
    library: &Path,
    entry: &str,
    image_modes: Option<u16>,
    filter_case_info: Option<Vec<FilterCaseInfo>>,
) -> PluginInfo {
    let path = library.to_string_lossy().into_owned();
    PluginInfo {
        path: path.clone(),
        binary: path,
        format: PluginFormat::Native,
        archs: vec!["native".into()],
        pipl: PiPL {
            kind: c::FILTER_KIND,
            name: entry.into(),
            category: "Tests".into(),
            entry_points: [("native".to_string(), entry.to_string())].into(),
            image_modes,
            filter_case_info,
            ..PiPL::default()
        },
        file_size: 0,
        file_mtime: 0.0,
        error: None,
        extra: BTreeMap::new(),
    }
}

/// Installs the test filter as a native plug-in (library + `.pipl` sidecar)
/// named `name` in `dir`, using entry point `entry`.
pub fn install_native(dir: &Path, name: &str, entry: &str) -> PathBuf {
    let library = test_filter().expect("test filter");
    let suffix = library.extension().and_then(|e| e.to_str()).unwrap_or("so");
    let target = dir.join(format!("{name}.{suffix}"));
    std::fs::copy(library, &target).expect("copy filter");
    let mut entry_bytes = entry.as_bytes().to_vec();
    entry_bytes.push(0);
    let pipl = build_pipl(
        &[
            (
                c::PIPL_KIND,
                int_property(c::FILTER_KIND as i32, ByteOrder::Little),
            ),
            (c::PIPL_NAME, pascal(name)),
            (c::PIPL_CATEGORY, pascal("Tests")),
            (c::PIPL_CODE_WIN64_X86, entry_bytes),
        ],
        ByteOrder::Little,
    );
    let mut sidecar = target.clone().into_os_string();
    sidecar.push(".pipl");
    std::fs::write(sidecar, pipl).expect("write sidecar");
    target
}

pub fn rgba_image(width: usize, height: usize, alpha: Option<u16>, depth: u32) -> PlaneImage {
    let mut data = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if depth == 8 {
                let a = alpha.map_or(((x * 40 + y) % 256) as u8, |a| a as u8);
                data.extend_from_slice(&[
                    ((x * 20) % 256) as u8,
                    ((y * 20) % 256) as u8,
                    ((x + y) % 256) as u8,
                    a,
                ]);
            } else {
                let a = alpha.unwrap_or(32768 - (x as u16) * 100);
                for value in [(x * 1000) as u16, (y * 1000) as u16, (x + y) as u16, a] {
                    data.extend_from_slice(&value.to_ne_bytes());
                }
            }
        }
    }
    PlaneImage::from_data(width, height, 4, depth, data).expect("image")
}

pub fn default_image() -> PlaneImage {
    rgba_image(6, 11, None, 8)
}

pub fn pixel(image: &PlaneImage, x: usize, y: usize) -> Vec<u32> {
    let start = y * image.row_bytes() + x * image.pixel_bytes();
    let raw = &image.data[start..start + image.pixel_bytes()];
    if image.depth == 8 {
        raw.iter().map(|&v| u32::from(v)).collect()
    } else {
        raw.chunks_exact(2)
            .map(|pair| u32::from(u16::from_ne_bytes([pair[0], pair[1]])))
            .collect()
    }
}
