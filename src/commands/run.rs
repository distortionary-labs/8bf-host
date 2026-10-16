//! `8bf-host run`: run a filter on an image file.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use image::ImageFormat;
use serde::Serialize;

use super::{execute, load_catalog};
use crate::catalog::filter_id;
use crate::cli::GlobalArgs;
use crate::error::{Error, Result};
use crate::host::image::{PlaneImage, Rect};
use crate::host::session::{FilterHost, FilterRequest, FilterResult, Hooks, RunStatus};
use crate::imageio::{self, LoadedImage};
use crate::preset::Preset;
use crate::runner::{self, Environment, Placement};

/// The path that means standard input or standard output.
const STDIO: &str = "-";

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Filter id, name, Category/Name, or plug-in file
    pub filter: String,

    /// Input image, or - for standard input
    pub input: PathBuf,

    /// Output image, or - for standard output
    pub output: PathBuf,

    /// Output format: png, jpeg, tiff, webp, bmp or pnm [default: from the output name, else the input's]
    #[arg(short, long, value_name = "FORMAT")]
    pub format: Option<String>,

    /// Show the filter's dialog (needs a desktop session)
    #[arg(short, long)]
    pub dialog: bool,

    /// Load the filter's settings from a preset file
    #[arg(short, long, value_name = "FILE")]
    pub preset: Option<PathBuf>,

    /// Save the settings the filter ran with to a preset file
    #[arg(short, long, value_name = "FILE")]
    pub save_preset: Option<PathBuf>,

    /// Selection mask: an image the size of the input; white is selected
    #[arg(short, long, value_name = "FILE")]
    pub mask: Option<PathBuf>,

    /// Only filter this rectangle: X,Y,WIDTH,HEIGHT in pixels
    #[arg(long, value_name = "X,Y,W,H", value_parser = parse_rect)]
    pub rect: Option<Rect>,

    /// Foreground color handed to the filter, as #rrggbb
    #[arg(long, value_name = "COLOR", value_parser = parse_color, default_value = "#000000")]
    pub foreground: [u8; 3],

    /// Background color handed to the filter, as #rrggbb
    #[arg(long, value_name = "COLOR", value_parser = parse_color, default_value = "#ffffff")]
    pub background: [u8; 3],

    /// Resolution reported to the filter, in pixels per inch
    #[arg(long, value_name = "DPI", value_parser = parse_resolution, default_value_t = 72.0)]
    pub resolution: f64,

    /// Fail instead of running 8-bit-only filters on an 8-bit copy of 16-bit images
    #[arg(long)]
    pub no_8bit_fallback: bool,

    /// Print a JSON summary of the run on standard output
    #[arg(long)]
    pub json: bool,

    /// Run the filter COUNT times, each on the last run's output and with
    /// its settings, as Photoshop's Last Filter command does
    #[arg(long, value_name = "COUNT", default_value_t = 1,
          value_parser = clap::value_parser!(u32).range(1..))]
    pub repeat: u32,

    /// Unload the filter's code after each run, as Photoshop does (in-process only)
    #[arg(long)]
    pub unload_between_runs: bool,

    /// Wait this many milliseconds after each run
    #[arg(long, value_name = "MS", default_value_t = 0)]
    pub pause: u64,
}

/// Parses `X,Y,W,H` into a rectangle.
pub(crate) fn parse_rect(text: &str) -> std::result::Result<Rect, String> {
    let values: Vec<i32> = text
        .split(',')
        .map(|part| part.trim().parse::<i32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| format!("expected X,Y,WIDTH,HEIGHT as integers, got '{text}'"))?;
    let [x, y, width, height] = values[..] else {
        return Err(format!(
            "expected four numbers X,Y,WIDTH,HEIGHT, got '{text}'"
        ));
    };
    if x < 0 || y < 0 || width <= 0 || height <= 0 {
        return Err("the rectangle needs X,Y >= 0 and a positive width and height".into());
    }
    let (Some(right), Some(bottom)) = (x.checked_add(width), y.checked_add(height)) else {
        return Err("the rectangle is too large".into());
    };
    Ok(Rect::new(y, x, bottom, right))
}

/// Parses `#rrggbb` (the `#` is optional).
pub(crate) fn parse_color(text: &str) -> std::result::Result<[u8; 3], String> {
    let trimmed = text.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    let invalid = || format!("expected a color like #ff8800, got '{text}'");
    // `from_str_radix` accepts a sign, so check the digits first.
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let channel =
        |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).map_err(|_| invalid());
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

/// Parses a resolution in pixels per inch: a finite, positive number.
pub(crate) fn parse_resolution(text: &str) -> std::result::Result<f64, String> {
    check_resolution(
        text.trim()
            .parse()
            .map_err(|_| format!("expected a number of pixels per inch, got '{text}'"))?,
    )
}

/// Accepts a resolution only when it is finite and positive.
pub(crate) fn check_resolution(resolution: f64) -> std::result::Result<f64, String> {
    if resolution.is_finite() && resolution > 0.0 {
        Ok(resolution)
    } else {
        Err(format!(
            "the resolution must be a positive number, not {resolution}"
        ))
    }
}

fn is_stdio(path: &Path) -> bool {
    path.as_os_str() == STDIO
}

fn read_input(path: &Path) -> Result<Vec<u8>> {
    if is_stdio(path) {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| Error::ImageRead {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        return Ok(bytes);
    }
    std::fs::read(path).map_err(|error| Error::ImageRead {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

fn output_format(args: &RunArgs, loaded: &LoadedImage) -> Result<ImageFormat> {
    if let Some(name) = &args.format {
        return imageio::format_from_name(name).ok_or_else(|| {
            Error::Usage(format!(
                "unknown output format '{name}' (use png, jpeg, tiff, webp, bmp or pnm)"
            ))
        });
    }
    if !is_stdio(&args.output) {
        if let Some(format) = imageio::format_from_name(&args.output.to_string_lossy()) {
            return Ok(format);
        }
        if args.output.extension().is_some() {
            return Err(Error::Usage(format!(
                "cannot tell the output format from '{}'; use --format",
                args.output.display()
            )));
        }
    }
    Ok(loaded
        .format
        .filter(|f| imageio::format_from_name(f.extensions_str()[0]).is_some())
        .unwrap_or(ImageFormat::Png))
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<()> {
    let written = if is_stdio(path) {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(bytes).and_then(|()| stdout.flush())
    } else {
        write_file(path, bytes)
    };
    written.map_err(|error| Error::ImageWrite {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

/// Writes `bytes` to `path`. An existing regular file (which may be the
/// input image) is replaced atomically, so a failed write such as a full
/// disk leaves it intact; it keeps its permissions, and a link to it stays
/// a link. Anything else, such as a new file or `/dev/null`, is written
/// directly.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let existing = std::fs::canonicalize(path)
        .and_then(|target| std::fs::metadata(&target).map(|metadata| (target, metadata)));
    let Ok((target, metadata)) = existing else {
        return std::fs::write(path, bytes);
    };
    if !metadata.is_file() {
        return std::fs::write(path, bytes);
    }
    let directory = target.parent().unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())?;
    temporary
        .persist(&target)
        .map(drop)
        .map_err(|error| error.error)
}

/// Installs a Ctrl+C handler: the first press asks the filter to stop,
/// the second exits at once.
fn interrupt_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&flag);
    let _ = ctrlc::set_handler(move || {
        if handler_flag.swap(true, Ordering::SeqCst) {
            std::process::exit(i32::from(crate::error::exit::CANCELLED));
        }
        eprintln!("\nstopping the filter (press Ctrl+C again to quit at once)");
    });
    flag
}

fn progress_hook(label: String, show: bool) -> Box<dyn FnMut(i32, i32)> {
    let mut last = None;
    Box::new(move |done, total| {
        if !show || total <= 0 {
            return;
        }
        let percent = (i64::from(done) * 100 / i64::from(total)).clamp(0, 100);
        if last != Some(percent) {
            last = Some(percent);
            eprint!("\r{label}: {percent:>3}%");
            let _ = std::io::stderr().flush();
        }
    })
}

#[derive(Serialize)]
struct Summary<'a> {
    status: RunStatus,
    filter: &'a str,
    id: String,
    output: String,
    ran: &'a str,
    runs: u32,
    preset_saved: Option<String>,
    log: &'a [String],
}

/// The image of a run that went well, or the error the run ended with.
fn finished_image(result: &FilterResult) -> Result<&PlaneImage> {
    match result.status {
        RunStatus::Cancelled => return Err(Error::Cancelled),
        RunStatus::Error => {
            return Err(Error::FilterFailed {
                message: result.message.clone(),
                code: result.code,
            })
        }
        RunStatus::Ok => {}
    }
    result.image.as_ref().ok_or_else(|| Error::FilterFailed {
        message: "the filter finished without an image".into(),
        code: 0,
    })
}

pub fn run(global: &GlobalArgs, args: &RunArgs) -> Result<()> {
    if args.json && is_stdio(&args.output) {
        return Err(Error::Usage(
            "--json prints to standard output, so the image cannot go there too".into(),
        ));
    }
    if is_stdio(&args.input) && args.mask.as_deref().is_some_and(is_stdio) {
        return Err(Error::Usage(
            "only one of the input and the mask can be '-'".into(),
        ));
    }
    let mut config = global.load_config()?;
    if args.no_8bit_fallback {
        config.allow_8bit_fallback = false;
    }
    let info = load_catalog(&config).resolve(&args.filter)?;
    let plan = runner::plan(&info, &config, &Environment::current())?;

    let bytes = read_input(&args.input)?;
    let loaded = imageio::decode(&bytes, None).map_err(|error| Error::ImageRead {
        path: args.input.clone(),
        reason: error.to_string(),
    })?;
    let format = output_format(args, &loaded)?;
    let mask = match &args.mask {
        Some(path) => Some(
            imageio::decode_mask(&read_input(path)?, loaded.image.width, loaded.image.height)
                .map_err(|error| Error::ImageRead {
                    path: path.clone(),
                    reason: error.to_string(),
                })?,
        ),
        None => None,
    };
    let preset = match &args.preset {
        Some(path) => Some(Preset::load(path).map_err(|error| Error::Usage(error.to_string()))?),
        None => None,
    };

    let mut request = FilterRequest::new(
        loaded.image.clone(),
        loaded.color_channels,
        loaded.has_alpha,
    );
    request.mask = mask;
    request.filter_rect = args.rect;
    request.foreground = args.foreground;
    request.background = args.background;
    request.resolution = args.resolution;
    request.show_ui = args.dialog;
    request.allow_8bit_fallback = config.allow_8bit_fallback;
    request.document_title = if is_stdio(&args.input) {
        "Untitled".into()
    } else {
        args.input
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    };
    request.layer_name = "Background".into();
    if let Some(preset) = preset {
        request.parameters = preset.parameters;
        request.descriptor = preset.descriptor;
    }

    let in_process = matches!(plan.placement, Placement::InProcess);
    if args.unload_between_runs && !in_process {
        return Err(Error::Usage(
            "--unload-between-runs needs the filter to run in-process (--run-mode in-process)"
                .into(),
        ));
    }

    let interrupted = interrupt_flag();
    let verbose = global.verbose;
    let show_progress = !global.quiet && std::io::stderr().is_terminal();
    let mut host = FilterHost::new();
    let mut result = None;
    for run in 1..=args.repeat {
        if let Some(previous) = result.take() {
            request.image = finished_image(&previous)?.clone();
            // Last Filter: the settings of the run before, no dialog. In
            // process the host still holds the filter's parameters; a
            // worker starts afresh and needs them handed over.
            request.show_ui = false;
            (request.parameters, request.descriptor) = if in_process {
                (None, None)
            } else {
                (previous.parameters, previous.descriptor)
            };
        }
        let label = if args.repeat > 1 {
            format!("{} ({run}/{})", info.name(), args.repeat)
        } else {
            info.name()
        };
        let abort_flag = Arc::clone(&interrupted);
        let hooks = Hooks {
            progress: progress_hook(label.clone(), show_progress),
            abort: Box::new(move || abort_flag.load(Ordering::SeqCst)),
            log: Box::new(move |line| {
                if verbose {
                    eprintln!("[host] {line}");
                }
            }),
        };
        if verbose {
            eprintln!("[host] running {label} {}", plan.description);
        }
        let ran = execute(&plan, &info, request.clone(), hooks, &mut host);
        if show_progress {
            eprintln!();
        }
        let mut stray_threads = 0;
        if args.unload_between_runs {
            for unloaded in host.unload(&info) {
                if !global.quiet {
                    eprintln!("[host] {}", unloaded.note);
                }
                stray_threads += unloaded.stray_threads;
            }
        }
        if args.pause > 0 {
            std::thread::sleep(Duration::from_millis(args.pause));
        }
        finished_image(&ran)?;
        if stray_threads > 0 {
            return Err(Error::FilterFailed {
                message: format!(
                    "{} left {stray_threads} threads running after it was unloaded; \
                     hosts that unload filters, as Photoshop does, crash when they wake",
                    info.name()
                ),
                code: 0,
            });
        }
        result = Some(ran);
    }
    let result = result.expect("--repeat is at least 1");
    let filtered = finished_image(&result)?;
    let image = loaded
        .to_dynamic(filtered)
        .map_err(|error| Error::FilterFailed {
            message: error.to_string(),
            code: 0,
        })?;
    let encoded = imageio::encode(image, format).map_err(|error| Error::ImageWrite {
        path: args.output.clone(),
        reason: error.to_string(),
    })?;
    write_output(&args.output, &encoded)?;

    if let Some(path) = &args.save_preset {
        let preset = Preset::from_run(&info, result.parameters.clone(), result.descriptor.clone());
        if preset.is_empty() && !global.quiet {
            eprintln!("warning: the filter reported no settings; the preset will be empty");
        }
        preset
            .save(path)
            .map_err(|error| Error::io(format!("could not write {}", path.display()), error))?;
    }
    if args.json {
        let summary = Summary {
            status: result.status,
            filter: &info.name(),
            id: filter_id(&info.key()),
            output: args.output.display().to_string(),
            ran: &plan.description,
            runs: args.repeat,
            preset_saved: args.save_preset.as_ref().map(|p| p.display().to_string()),
            log: &result.log,
        };
        let text = serde_json::to_string_pretty(&summary)
            .map_err(|error| Error::Usage(error.to_string()))?;
        println!("{text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_files_are_created_or_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.png");
        write_file(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_file(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        // Only the output is left behind, no temporary files.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_an_output_keeps_its_permissions_and_links() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("image.png");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_file(&link, b"new").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[test]
    fn rects_are_x_y_width_height() {
        assert_eq!(parse_rect("10,20,30,40"), Ok(Rect::new(20, 10, 60, 40)));
        assert_eq!(parse_rect(" 0, 0, 1, 1 "), Ok(Rect::new(0, 0, 1, 1)));
    }

    #[test]
    fn bad_rects_are_rejected() {
        for bad in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "a,b,c,d",
            "-1,0,5,5",
            "0,0,0,5",
            "1,0,2147483647,1",
        ] {
            assert!(parse_rect(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn colors_parse_with_or_without_a_hash() {
        assert_eq!(parse_color("#ff8000"), Ok([255, 128, 0]));
        assert_eq!(parse_color("00FF10"), Ok([0, 255, 16]));
        for bad in [
            "#fff", "#gg0000", "#ff00001", "é12345", "#+f+f+f", "##ff8000",
        ] {
            assert!(parse_color(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resolutions_must_be_positive_and_finite() {
        assert_eq!(parse_resolution(" 300 "), Ok(300.0));
        for bad in ["0", "-72", "NaN", "inf", "dpi"] {
            assert!(parse_resolution(bad).is_err(), "{bad}");
        }
    }
}
