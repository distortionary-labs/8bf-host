//! Running a filter in another process: request and response files.
//!
//! The parent writes a job directory holding `request.json` and the raw
//! pixel files, then starts a worker (`8bf-host worker request.json`):
//! this binary, a build of it for another architecture, or a Windows build
//! under Wine. Files inside the job are named relative to the job
//! directory, so only the request path and the plug-in paths need
//! translating for Wine.
//!
//! While it runs, the worker prints `PROGRESS <done> <total>` lines on
//! stdout. Creating the job's `cancel` file asks the filter to stop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::constants as c;
use crate::host::descriptors::Descriptor;
use crate::host::discovery::{read_capped, PluginInfo};
use crate::host::image::{PlaneImage, Rect};
use crate::host::session::{FilterHost, FilterRequest, FilterResult, Hooks, RunStatus};

pub const REQUEST_VERSION: u32 = 1;
pub const REQUEST_NAME: &str = "request.json";
pub const RESPONSE_NAME: &str = "response.json";
pub const INPUT_NAME: &str = "input.bin";
pub const MASK_NAME: &str = "mask.bin";
pub const OUTPUT_NAME: &str = "output.bin";
pub const PARAMETERS_IN_NAME: &str = "parameters-in.bin";
pub const PARAMETERS_OUT_NAME: &str = "parameters-out.bin";
pub const CANCEL_NAME: &str = "cancel";

/// How often the worker looks for the cancel file.
/// The largest `response.json` the parent reads. It holds a status, a
/// message, the filter's log and a descriptor: megabytes at most.
const MAX_RESPONSE_FILE: u64 = 64 * 1024 * 1024;
/// The largest parameters file either side reads: the largest handle a
/// filter can allocate.
const MAX_PARAMETERS_FILE: u64 = crate::host::memory::MAX_ALLOCATION as u64;
/// Image files are checked against their stated size once read; this only
/// keeps a wrong file from being read whole first.
const MAX_IMAGE_FILE: u64 = isize::MAX as u64;

const CANCEL_POLL: Duration = Duration::from_millis(100);

/// The worker's exit code when the run succeeded or was cancelled.
pub const WORKER_OK: i32 = 0;
/// The worker's exit code when the filter failed; the response says why.
pub const WORKER_FAILED: i32 = 1;

/// What the worker should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Run,
    About,
}

#[derive(Serialize, Deserialize)]
struct ImageSpec {
    width: usize,
    height: usize,
    channels: usize,
    depth: u32,
    file: String,
}

#[derive(Serialize, Deserialize)]
struct JobRequest {
    version: u32,
    action: Action,
    plugin: PluginInfo,
    arch: Option<String>,
    image: ImageSpec,
    color_channels: usize,
    has_alpha: bool,
    mask: Option<String>,
    filter_rect: Option<Rect>,
    foreground: [u8; 3],
    background: [u8; 3],
    resolution: f64,
    show_ui: bool,
    parameters_file: Option<String>,
    descriptor: Option<Value>,
    parent_window: isize,
    document_title: String,
    layer_name: String,
    allow_8bit_fallback: bool,
}

#[derive(Serialize, Deserialize)]
struct JobResponse {
    status: RunStatus,
    message: String,
    code: i16,
    log: Vec<String>,
    #[serde(default)]
    parameters_file: Option<String>,
    #[serde(default)]
    descriptor: Option<Value>,
}

fn write_file(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(dir.join(name), data)
}

/// Writes a job for `info` and `request` into `dir` and returns the request path.
///
/// `info` may already have its paths translated for the worker.
///
/// # Errors
///
/// Fails when the job files cannot be written.
pub fn write_job(
    dir: &Path,
    info: &PluginInfo,
    request: &FilterRequest,
    action: Action,
    arch: Option<&str>,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let image = &request.image;
    write_file(dir, INPUT_NAME, &image.data)?;
    let mask = match &request.mask {
        Some(mask) => {
            write_file(dir, MASK_NAME, &mask.data)?;
            Some(MASK_NAME.to_string())
        }
        None => None,
    };
    let parameters_file = match &request.parameters {
        Some(parameters) => {
            write_file(dir, PARAMETERS_IN_NAME, parameters)?;
            Some(PARAMETERS_IN_NAME.to_string())
        }
        None => None,
    };
    let payload = JobRequest {
        version: REQUEST_VERSION,
        action,
        plugin: info.clone(),
        arch: arch.map(str::to_string),
        image: ImageSpec {
            width: image.width,
            height: image.height,
            channels: image.channels,
            depth: image.depth,
            file: INPUT_NAME.into(),
        },
        color_channels: request.color_channels,
        has_alpha: request.has_alpha,
        mask,
        filter_rect: request.filter_rect,
        foreground: request.foreground,
        background: request.background,
        resolution: request.resolution,
        show_ui: request.show_ui,
        parameters_file,
        descriptor: request.descriptor.as_ref().map(Descriptor::to_json),
        parent_window: request.parent_window,
        document_title: request.document_title.clone(),
        layer_name: request.layer_name.clone(),
        allow_8bit_fallback: request.allow_8bit_fallback,
    };
    let path = dir.join(REQUEST_NAME);
    let text = serde_json::to_vec(&payload).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Reads a finished job's response.
///
/// A missing or unreadable response becomes an error result; the caller
/// adds the worker's stderr to explain it.
pub fn read_response(dir: &Path, request: &FilterRequest) -> FilterResult {
    // The worker runs filter code, so its files are untrusted: each read
    // is capped and refuses anything but a regular file (a FIFO would
    // block, a link to a device would never end).
    let response = read_capped(&dir.join(RESPONSE_NAME), MAX_RESPONSE_FILE)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<JobResponse>(&bytes).map_err(|error| error.to_string())
        });
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return FilterResult::error(
                format!("The filter worker did not produce a result ({error})."),
                c::PARAM_ERR,
            )
        }
    };
    let mut result = FilterResult {
        status: response.status,
        message: response.message,
        code: response.code,
        log: response.log,
        ..FilterResult::error("", 0)
    };
    if result.status != RunStatus::Ok {
        return result;
    }
    let source = &request.image;
    let expected = u64::try_from(source.data.len()).unwrap_or(u64::MAX);
    let image = read_capped(&dir.join(OUTPUT_NAME), expected)
        .map_err(|error| error.to_string())
        .and_then(|data| {
            PlaneImage::from_data(
                source.width,
                source.height,
                source.channels,
                source.depth,
                data,
            )
            .map_err(|error| error.to_string())
        });
    match image {
        Ok(image) => result.image = Some(image),
        Err(error) => {
            return FilterResult::error(
                format!("The filter worker's output is unusable: {error}"),
                c::PARAM_ERR,
            )
        }
    }
    // The image is the result; settings that can't be read back are lost
    // for presets, which the log says rather than failing the run.
    if let Some(name) = response.parameters_file {
        match read_job_file(dir, &name, MAX_PARAMETERS_FILE) {
            Ok(parameters) => result.parameters = Some(parameters),
            Err(error) => result
                .log
                .push(format!("the filter's parameters were lost: {error}")),
        }
    }
    if let Some(value) = response.descriptor {
        match Descriptor::from_json(&value) {
            Ok(descriptor) => result.descriptor = Some(descriptor),
            Err(error) => result
                .log
                .push(format!("the filter's descriptor was lost: {error}")),
        }
    }
    result
}

/// Job files are plain names inside the job directory; never follow a path
/// the other side supplied out of it.
fn is_job_file(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', ':']) && name != ".." && name != "."
}

/// Checks for the cancel file at most every [`CANCEL_POLL`].
struct CancelWatcher {
    path: PathBuf,
    checked: Option<Instant>,
    cancelled: bool,
}

impl CancelWatcher {
    fn poll(&mut self) -> bool {
        if self.cancelled {
            return true;
        }
        if self.checked.is_some_and(|at| at.elapsed() < CANCEL_POLL) {
            return false;
        }
        self.checked = Some(Instant::now());
        self.cancelled = self.path.exists();
        self.cancelled
    }
}

fn read_job_file(dir: &Path, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    if !is_job_file(name) {
        return Err(format!("invalid job file name {name:?}"));
    }
    read_capped(&dir.join(name), limit).map_err(|error| format!("could not read {name}: {error}"))
}

fn request_from_job(dir: &Path, job: &JobRequest) -> Result<FilterRequest, String> {
    let spec = &job.image;
    let data = read_job_file(dir, &spec.file, MAX_IMAGE_FILE)?;
    let image = PlaneImage::from_data(spec.width, spec.height, spec.channels, spec.depth, data)
        .map_err(|error| error.to_string())?;
    let mask = match &job.mask {
        Some(name) => Some(
            PlaneImage::from_data(
                image.width,
                image.height,
                1,
                8,
                read_job_file(dir, name, MAX_IMAGE_FILE)?,
            )
            .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    let parameters = match &job.parameters_file {
        Some(name) => Some(read_job_file(dir, name, MAX_PARAMETERS_FILE)?),
        None => None,
    };
    let descriptor = match &job.descriptor {
        Some(value) => Some(Descriptor::from_json(value)?),
        None => None,
    };
    Ok(FilterRequest {
        image,
        color_channels: job.color_channels,
        has_alpha: job.has_alpha,
        mask,
        filter_rect: job.filter_rect,
        foreground: job.foreground,
        background: job.background,
        resolution: job.resolution,
        show_ui: job.show_ui,
        parameters,
        descriptor,
        parent_window: job.parent_window,
        document_title: job.document_title.clone(),
        layer_name: job.layer_name.clone(),
        allow_8bit_fallback: job.allow_8bit_fallback,
    })
}

fn respond(dir: &Path, result: &FilterResult) -> i32 {
    let mut response = JobResponse {
        status: result.status,
        message: result.message.clone(),
        code: result.code,
        log: result.log.clone(),
        parameters_file: None,
        descriptor: None,
    };
    if let Some(image) = &result.image {
        if let Err(error) = write_file(dir, OUTPUT_NAME, &image.data) {
            response.status = RunStatus::Error;
            response.message = format!("could not write the result: {error}");
        }
    }
    if let Some(parameters) = &result.parameters {
        if write_file(dir, PARAMETERS_OUT_NAME, parameters).is_ok() {
            response.parameters_file = Some(PARAMETERS_OUT_NAME.into());
        }
    }
    response.descriptor = result.descriptor.as_ref().map(Descriptor::to_json);
    let written = serde_json::to_vec(&response)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(dir.join(RESPONSE_NAME), bytes));
    if written.is_err() || response.status == RunStatus::Error {
        return WORKER_FAILED;
    }
    WORKER_OK
}

/// Executes the job described by a request file. Returns the worker's exit code.
///
/// Progress goes to `progress` as `PROGRESS <done> <total>` lines; host log
/// lines go to stderr.
pub fn run_job(request_path: &Path, progress: Box<dyn Write>) -> i32 {
    let request_path = std::path::absolute(request_path).unwrap_or(request_path.to_path_buf());
    let dir = request_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let job = std::fs::read(&request_path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<JobRequest>(&bytes).map_err(|error| error.to_string())
        });
    let job = match job {
        Ok(job) if job.version == REQUEST_VERSION => job,
        Ok(_) => {
            return respond(
                &dir,
                &FilterResult::error("Unsupported request version.", c::PARAM_ERR),
            )
        }
        Err(error) => {
            return respond(
                &dir,
                &FilterResult::error(format!("Unreadable request: {error}"), c::PARAM_ERR),
            )
        }
    };
    crate::host::macapp::ensure_application();
    let mut host = FilterHost::new();
    let log: Box<dyn FnMut(&str)> = Box::new(|line| eprintln!("{line}"));
    if job.action == Action::About {
        let result = host.about(&job.plugin, job.parent_window, log, job.arch.as_deref());
        return respond(&dir, &result);
    }
    let request = match request_from_job(&dir, &job) {
        Ok(request) => request,
        Err(error) => return respond(&dir, &FilterResult::error(error, c::PARAM_ERR)),
    };
    let mut watcher = CancelWatcher {
        path: dir.join(CANCEL_NAME),
        checked: None,
        cancelled: false,
    };
    let hooks = Hooks {
        progress: progress_printer(progress),
        abort: Box::new(move || watcher.poll()),
        log,
    };
    let result = host.run(&job.plugin, request, hooks, job.arch.as_deref());
    respond(&dir, &result)
}

/// Prints a progress line whenever the whole-percent value changes.
fn progress_printer(mut out: Box<dyn Write>) -> Box<dyn FnMut(i32, i32)> {
    let mut last = None;
    Box::new(move |done, total| {
        if total <= 0 {
            return;
        }
        let percent = i64::from(done) * 100 / i64::from(total);
        if last != Some(percent) {
            last = Some(percent);
            let _ = writeln!(out, "PROGRESS {done} {total}");
            let _ = out.flush();
        }
    })
}

/// Parses a `PROGRESS <done> <total>` line.
pub fn parse_progress(line: &str) -> Option<(i32, i32)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "PROGRESS" {
        return None;
    }
    let done = parts.next()?.parse().ok()?;
    let total = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((done, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::discovery::PluginFormat;
    use crate::host::pipl::PiPL;
    use std::collections::BTreeMap;

    fn info() -> PluginInfo {
        PluginInfo {
            path: "/p/a.so".into(),
            binary: "/p/a.so".into(),
            format: PluginFormat::Native,
            archs: vec!["native".into()],
            pipl: PiPL::default(),
            file_size: 0,
            file_mtime: 0.0,
            error: None,
            extra: BTreeMap::new(),
        }
    }

    fn request() -> FilterRequest {
        let image = PlaneImage::from_data(2, 1, 4, 8, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let mut request = FilterRequest::new(image, 3, true);
        request.mask = Some(PlaneImage::from_data(2, 1, 1, 8, vec![255, 0]).unwrap());
        request.parameters = Some(vec![9, 9]);
        request.filter_rect = Some(Rect::new(0, 0, 1, 2));
        request
    }

    #[test]
    fn jobs_round_trip_through_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_job(dir.path(), &info(), &request(), Action::Run, Some("native")).unwrap();
        let job: JobRequest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let rebuilt = request_from_job(dir.path(), &job).unwrap();
        let original = request();
        assert_eq!(rebuilt.image, original.image);
        assert_eq!(rebuilt.mask, original.mask);
        assert_eq!(rebuilt.parameters, original.parameters);
        assert_eq!(rebuilt.filter_rect, original.filter_rect);
    }

    #[test]
    fn responses_carry_images_and_parameters() {
        let dir = tempfile::tempdir().unwrap();
        let mut result = FilterResult::error("", 0);
        result.status = RunStatus::Ok;
        result.image = Some(request().image);
        result.parameters = Some(vec![1]);
        assert_eq!(respond(dir.path(), &result), WORKER_OK);
        let read = read_response(dir.path(), &request());
        assert!(read.is_ok());
        assert_eq!(read.image, Some(request().image));
        assert_eq!(read.parameters, Some(vec![1]));
    }

    #[test]
    fn missing_or_failed_responses_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let read = read_response(dir.path(), &request());
        assert_eq!(read.status, RunStatus::Error);
        assert_eq!(
            respond(dir.path(), &FilterResult::error("boom", c::PARAM_ERR)),
            WORKER_FAILED
        );
        assert_eq!(read_response(dir.path(), &request()).message, "boom");
    }

    #[test]
    fn unreadable_settings_are_noted_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut result = FilterResult::error("", 0);
        result.status = RunStatus::Ok;
        result.image = Some(request().image);
        respond(dir.path(), &result);
        let path = dir.path().join(RESPONSE_NAME);
        let mut response: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        response["parameters_file"] = "../escape.bin".into();
        response["descriptor"] = "not a descriptor".into();
        std::fs::write(&path, response.to_string()).unwrap();
        let read = read_response(dir.path(), &request());
        assert!(read.is_ok());
        assert_eq!(read.parameters, None);
        assert_eq!(read.log.len(), 2, "{:?}", read.log);
    }

    #[test]
    fn oversized_or_special_output_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut result = FilterResult::error("", 0);
        result.status = RunStatus::Ok;
        result.image = Some(request().image);
        respond(dir.path(), &result);
        let output = dir.path().join(OUTPUT_NAME);
        let mut data = std::fs::read(&output).unwrap();
        data.push(0);
        std::fs::write(&output, &data).unwrap();
        assert_eq!(
            read_response(dir.path(), &request()).status,
            RunStatus::Error
        );

        #[cfg(unix)]
        {
            std::fs::remove_file(&output).unwrap();
            std::os::unix::fs::symlink("/dev/zero", &output).unwrap();
            assert_eq!(
                read_response(dir.path(), &request()).status,
                RunStatus::Error
            );
        }
    }

    #[test]
    fn job_file_names_cannot_escape_the_job_directory() {
        assert!(is_job_file("input.bin"));
        for bad in ["", ".", "..", "../x", "a/b", "C:x", r"a\b"] {
            assert!(!is_job_file(bad), "{bad}");
        }
    }

    #[test]
    fn progress_lines_parse() {
        assert_eq!(parse_progress("PROGRESS 3 10\n"), Some((3, 10)));
        assert_eq!(parse_progress("PROGRESS 3"), None);
        assert_eq!(parse_progress("other 1 2"), None);
        assert_eq!(parse_progress("PROGRESS 1 2 3"), None);
    }

    #[test]
    fn unreadable_requests_get_an_error_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(REQUEST_NAME);
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(run_job(&path, Box::new(std::io::sink())), WORKER_FAILED);
        assert_eq!(
            read_response(dir.path(), &request()).status,
            RunStatus::Error
        );
    }
}
