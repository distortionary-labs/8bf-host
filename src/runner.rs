//! Deciding where a filter runs, and running it in a worker process.
//!
//! A filter has to run in a process whose operating system and CPU
//! architecture match its binary:
//!
//! * **in-process**: this process loads the filter. Fastest, and the
//!   filter's dialog belongs to this process. A crashing filter takes the
//!   process down with it.
//! * **worker**: a separate `8bf-host worker` process runs the job. It is
//!   used for crash isolation (`run_mode = "worker"`, and the server's
//!   default), for 32-bit filters on 64-bit Windows, for Intel-only filters
//!   on Apple silicon (under Rosetta), and for Windows filters on Linux,
//!   where a Windows build of this program runs under Wine.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::{Config, RunMode};
use crate::error::{Error, Result};
use crate::host::constants as c;
use crate::host::discovery::{host_arch, PluginInfo, NATIVE_ARCH};
use crate::host::session::{FilterRequest, FilterResult, Hooks, RunStatus};
use crate::protocol::{self, Action};

/// How long a worker gets to stop after being asked to cancel.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
/// How long to wait for the worker's output pipes to close once it has
/// exited. A process it started (such as `wineserver`) can inherit them
/// and keep them open indefinitely.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// How often the parent checks on a running worker.
const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// How many lines of worker stderr to quote when it fails without a response.
const STDERR_TAIL: usize = 15;
/// How much of the worker's stderr is kept: the end, where the failure is.
/// A filter that logs in a loop would otherwise grow the parent without
/// bound and fill the result's log.
const STDERR_LIMIT: usize = 1024 * 1024;
/// The longest progress line read from the worker; longer lines are skipped.
const PROGRESS_LINE_LIMIT: usize = 256;

/// File names looked for next to this executable when no worker is configured.
pub const WORKER_WIN_X64: &str = "8bf-host-win-x64.exe";
pub const WORKER_WIN_X86: &str = "8bf-host-win-x86.exe";
pub const WORKER_MAC_X64: &str = "8bf-host-mac-x64";

/// The operating system family, for planning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    /// Linux and other Unix systems: Windows filters go through Wine.
    Unix,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Unix
        }
    }
}

/// What the planner needs to know about the running program.
#[derive(Clone, Debug)]
pub struct Environment {
    pub platform: Platform,
    pub host_arch: String,
    /// This executable, used as the worker for same-architecture isolation.
    pub self_exe: PathBuf,
}

impl Environment {
    pub fn current() -> Self {
        Self {
            platform: Platform::current(),
            host_arch: host_arch().to_string(),
            self_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("8bf-host")),
        }
    }
}

/// Where a filter will run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    InProcess,
    Worker {
        /// The program and the arguments before `worker <request>`.
        command: Vec<OsString>,
        env: Vec<(String, String)>,
        /// Paths handed to the worker are translated to Wine's `Z:` drive.
        wine: bool,
    },
}

/// The decision for one filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub arch: String,
    pub placement: Placement,
    /// Where the filter runs, for people: "in this process (win-x64)".
    pub description: String,
}

/// Maps a Unix path to the `Z:` drive Wine exposes for the root filesystem.
///
/// # Examples
///
/// ```
/// # #[cfg(unix)] {
/// use std::path::Path;
/// assert_eq!(eightbf_host::runner::wine_path(Path::new("/tmp/a b")), r"Z:\tmp\a b");
/// # }
/// ```
pub fn wine_path(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!("Z:{}", absolute.to_string_lossy().replace('/', "\\"))
}

fn looks_windows(path: &Path) -> bool {
    path.to_string_lossy().as_bytes().get(1) == Some(&b':')
}

/// The configured worker build for `arch`, or the conventional file next to
/// this executable.
fn worker_binary(config: &Config, env: &Environment, arch: &str) -> Option<PathBuf> {
    let (configured, sibling) = match arch {
        "win-x64" => (&config.workers.win_x64, WORKER_WIN_X64),
        "win-x86" => (&config.workers.win_x86, WORKER_WIN_X86),
        "mac-x64" => (&config.workers.mac_x64, WORKER_MAC_X64),
        _ => return None,
    };
    if let Some(path) = configured {
        return Some(path.clone());
    }
    let candidate = env.self_exe.parent()?.join(sibling);
    candidate.is_file().then_some(candidate)
}

fn worker_args(program: impl Into<OsString>) -> Vec<OsString> {
    vec![program.into()]
}

/// Chooses how to run `info`.
///
/// # Errors
///
/// Returns [`Error::CannotRun`] with an explanation of what is missing
/// when the filter cannot run with this configuration.
pub fn plan(info: &PluginInfo, config: &Config, env: &Environment) -> Result<Plan> {
    let runnable = info.runnable_archs();
    if runnable.is_empty() {
        let entry_points: Vec<&str> = info.pipl.entry_points.keys().map(String::as_str).collect();
        return Err(Error::CannotRun(format!(
            "{} has no code this host can run (binary: {}; entry points: {}).",
            info.name(),
            if info.archs.is_empty() {
                "unknown".into()
            } else {
                info.archs.join(", ")
            },
            if entry_points.is_empty() {
                "none".into()
            } else {
                entry_points.join(", ")
            },
        )));
    }
    // Native plug-ins (a library plus a `.pipl` sidecar) are built for the
    // running OS by definition, so they run locally on every platform.
    let local = [env.host_arch.as_str(), NATIVE_ARCH]
        .into_iter()
        .find(|arch| runnable.iter().any(|candidate| candidate == arch));
    if let Some(arch) = local {
        let placement = if config.run_mode == RunMode::Worker {
            Placement::Worker {
                command: worker_args(&env.self_exe),
                env: Vec::new(),
                wine: false,
            }
        } else {
            Placement::InProcess
        };
        let description = match placement {
            Placement::InProcess => format!("in this process ({arch})"),
            Placement::Worker { .. } => format!("in a worker process ({arch})"),
        };
        return Ok(Plan {
            arch: arch.to_string(),
            placement,
            description,
        });
    }
    if config.run_mode == RunMode::InProcess {
        return Err(Error::CannotRun(format!(
            "{} is not built for {} (it has {}); set run_mode to auto to use a worker.",
            info.name(),
            env.host_arch,
            runnable.join(", ")
        )));
    }
    match env.platform {
        Platform::Windows => plan_windows(info, config, env, &runnable),
        Platform::MacOs => plan_mac(info, config, env, &runnable),
        Platform::Unix => plan_wine(info, config, env, &runnable),
    }
}

fn plan_windows(
    info: &PluginInfo,
    config: &Config,
    env: &Environment,
    runnable: &[String],
) -> Result<Plan> {
    for arch in ["win-x64", "win-x86"] {
        if !runnable.iter().any(|candidate| candidate == arch) {
            continue;
        }
        if let Some(program) = worker_binary(config, env, arch) {
            return Ok(Plan {
                arch: arch.into(),
                placement: Placement::Worker {
                    command: worker_args(&program),
                    env: Vec::new(),
                    wine: false,
                },
                description: format!("in {} ({arch})", program.display()),
            });
        }
    }
    Err(Error::CannotRun(format!(
        "{} is a {} filter. Install the matching build of 8bf-host ({}) next to this one, \
         or set [workers] in the config file.",
        info.name(),
        runnable.join(" / "),
        if runnable.iter().any(|a| a == "win-x86") {
            WORKER_WIN_X86
        } else {
            WORKER_WIN_X64
        },
    )))
}

fn plan_mac(
    info: &PluginInfo,
    config: &Config,
    env: &Environment,
    runnable: &[String],
) -> Result<Plan> {
    let intel = runnable.iter().any(|arch| arch == "mac-x64");
    if intel && env.host_arch == "mac-arm64" {
        // A universal build of this program can serve as its own Intel worker.
        let program = worker_binary(config, env, "mac-x64").unwrap_or_else(|| env.self_exe.clone());
        let mut command = worker_args("arch");
        command.push("-x86_64".into());
        command.push(program.clone().into_os_string());
        return Ok(Plan {
            arch: "mac-x64".into(),
            placement: Placement::Worker {
                command,
                env: Vec::new(),
                wine: false,
            },
            description: format!("under Rosetta ({})", program.display()),
        });
    }
    Err(Error::CannotRun(format!(
        "{} is not built for this Mac ({}; it has {}).",
        info.name(),
        env.host_arch,
        runnable.join(", ")
    )))
}

fn plan_wine(
    info: &PluginInfo,
    config: &Config,
    env: &Environment,
    runnable: &[String],
) -> Result<Plan> {
    for arch in ["win-x64", "win-x86"] {
        if !runnable.iter().any(|candidate| candidate == arch) {
            continue;
        }
        let Some(program) = worker_binary(config, env, arch) else {
            continue;
        };
        let mut variables = vec![("WINEDEBUG".to_string(), "-all".to_string())];
        if let Some(prefix) = &config.wine.prefix {
            variables.push(("WINEPREFIX".into(), prefix.to_string_lossy().into_owned()));
        }
        let mut command = worker_args(&config.wine.command);
        if looks_windows(&program) {
            command.push(program.clone().into_os_string());
        } else {
            command.push(wine_path(&program).into());
        }
        return Ok(Plan {
            arch: arch.into(),
            placement: Placement::Worker {
                command,
                env: variables,
                wine: true,
            },
            description: format!("under Wine ({arch}, {})", program.display()),
        });
    }
    if runnable.iter().any(|arch| arch.starts_with("win-")) {
        let wanted = if runnable.iter().any(|a| a == "win-x64") {
            "win_x64"
        } else {
            "win_x86"
        };
        return Err(Error::CannotRun(format!(
            "{} is a Windows filter. Install Wine and the Windows build of 8bf-host, then set \
             workers.{wanted} in the config file (or put {} next to this program).",
            info.name(),
            if wanted == "win_x64" {
                WORKER_WIN_X64
            } else {
                WORKER_WIN_X86
            },
        )));
    }
    Err(Error::CannotRun(format!(
        "{} cannot run on this system (it has {}).",
        info.name(),
        runnable.join(", ")
    )))
}

fn translated(info: &PluginInfo, wine: bool) -> PluginInfo {
    if !wine {
        return info.clone();
    }
    let mut translated = info.clone();
    translated.path = wine_path(Path::new(&info.path));
    translated.binary = wine_path(Path::new(&info.binary));
    translated
}

/// Runs a job in a worker process according to `plan`.
///
/// `hooks.progress` and `hooks.abort` work as they do in-process. Worker
/// stderr is passed to `hooks.log` and kept in the result's log.
pub fn run_worker(
    plan: &Plan,
    info: &PluginInfo,
    request: &FilterRequest,
    mut hooks: Hooks,
    action: Action,
    job_root: Option<&Path>,
) -> FilterResult {
    let Placement::Worker { command, env, wine } = &plan.placement else {
        return FilterResult::error("run_worker needs a worker plan", c::PARAM_ERR);
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix("8bf-host-");
    let job = match job_root {
        Some(root) => builder.tempdir_in(root),
        None => builder.tempdir(),
    };
    let job = match job {
        Ok(job) => job,
        Err(error) => {
            return FilterResult::error(
                format!("Could not create a job directory: {error}"),
                c::WRITE_ERR,
            )
        }
    };
    let child_info = translated(info, *wine);
    let request_path =
        match protocol::write_job(job.path(), &child_info, request, action, Some(&plan.arch)) {
            Ok(path) => path,
            Err(error) => {
                return FilterResult::error(
                    format!("Could not write the job files: {error}"),
                    c::WRITE_ERR,
                )
            }
        };
    let request_arg: OsString = if *wine {
        wine_path(&request_path).into()
    } else {
        request_path.into_os_string()
    };
    let Some((program, args)) = command.split_first() else {
        return FilterResult::error("empty worker command", c::PARAM_ERR);
    };
    let mut process = Command::new(program);
    process
        .args(args)
        .arg("worker")
        .arg(&request_arg)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .current_dir(job.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut process);
    (hooks.log)(&format!("starting worker: {}", program.to_string_lossy()));
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return FilterResult::error(
                format!(
                    "Could not start the filter worker ({}): {error}",
                    program.to_string_lossy()
                ),
                c::PARAM_ERR,
            )
        }
    };

    let (progress_tx, progress_rx) = mpsc::channel();
    let stdout = child.stdout.take();
    std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            for_each_short_line(stdout, PROGRESS_LINE_LIMIT, |line| {
                if let Some(progress) = protocol::parse_progress(line) {
                    let _ = progress_tx.send(progress);
                }
            });
        }
    });
    let stderr = child.stderr.take();
    let (stderr_tx, stderr_rx) = mpsc::channel();
    // Neither reader is joined: both report over their channels, so a pipe
    // held open by a stray process can't hold up the result.
    std::thread::spawn(move || {
        let _ = stderr_tx
            .send(stderr.map_or_else(String::new, |stderr| read_tail(stderr, STDERR_LIMIT)));
    });

    let mut cancel_sent: Option<Instant> = None;
    let exit = loop {
        for (done, total) in progress_rx.try_iter() {
            (hooks.progress)(done, total);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                (hooks.log)(&format!("lost track of the filter worker: {error}"));
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
        if cancel_sent.is_none() && (hooks.abort)() {
            let _ = std::fs::write(job.path().join(protocol::CANCEL_NAME), b"");
            cancel_sent = Some(Instant::now());
        }
        if cancel_sent.is_some_and(|at| at.elapsed() > CANCEL_GRACE) {
            let _ = child.kill();
            let _ = child.wait();
            return FilterResult {
                status: RunStatus::Cancelled,
                message: "Cancelled.".into(),
                code: c::USER_CANCELED_ERR,
                ..FilterResult::error("", 0)
            };
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    let pipes_deadline = Instant::now() + PIPE_GRACE;
    let remaining = || pipes_deadline.saturating_duration_since(Instant::now());
    loop {
        match progress_rx.recv_timeout(remaining()) {
            Ok((done, total)) => (hooks.progress)(done, total),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                (hooks.log)("the filter worker's output stayed open after it exited");
                break;
            }
        }
    }
    let stderr = stderr_rx.recv_timeout(remaining()).unwrap_or_else(|_| {
        (hooks.log)("the filter worker's error output stayed open after it exited");
        String::new()
    });
    let stderr_lines: Vec<&str> = stderr.lines().collect();
    let has_response = job.path().join(protocol::RESPONSE_NAME).exists();
    let mut result = protocol::read_response(job.path(), request);
    for line in &stderr_lines {
        (hooks.log)(line);
    }
    // The worker's own log is already in its stderr; keep one copy, plus
    // any notes `read_response` added on this side.
    let from_worker: std::collections::HashSet<&str> = stderr_lines.iter().copied().collect();
    let notes: Vec<String> = std::mem::take(&mut result.log)
        .into_iter()
        .filter(|line| !from_worker.contains(line.as_str()))
        .collect();
    for note in &notes {
        (hooks.log)(note);
    }
    result.log = stderr_lines.iter().map(|line| line.to_string()).collect();
    result.log.extend(notes);
    if !has_response {
        let code = exit
            .and_then(|status| status.code())
            .map_or_else(|| "unknown".to_string(), |code| code.to_string());
        let tail = stderr_lines[stderr_lines.len().saturating_sub(STDERR_TAIL)..].join("\n");
        result.message = format!(
            "The filter worker exited with code {code} without a result.{}",
            if tail.trim().is_empty() {
                String::new()
            } else {
                format!("\n\n{}", tail.trim())
            }
        );
    }
    result
}

/// Calls `handle` with each line of `input` up to `limit` bytes long.
///
/// Longer lines are dropped whole rather than buffered, so a worker that
/// writes without newlines cannot make the parent hold its output.
fn for_each_short_line(input: impl Read, limit: usize, mut handle: impl FnMut(&str)) {
    let mut reader = BufReader::new(input);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Read one byte past the limit to tell a full line from a long one.
        let read = (&mut reader)
            .take(limit as u64 + 1)
            .read_until(b'\n', &mut line);
        match read {
            Ok(0) | Err(_) => return,
            Ok(_) if line.ends_with(b"\n") => handle(&String::from_utf8_lossy(&line)),
            Ok(_) if line.len() <= limit => {
                // The last line, without a newline.
                handle(&String::from_utf8_lossy(&line));
                return;
            }
            Ok(_) => {
                // Too long: discard up to the next newline.
                let mut rest = Vec::new();
                loop {
                    rest.clear();
                    match (&mut reader).take(64 * 1024).read_until(b'\n', &mut rest) {
                        Ok(0) | Err(_) => return,
                        Ok(_) if rest.ends_with(b"\n") => break,
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}

/// Reads `input` to the end, keeping only its last `limit` bytes.
///
/// The text starts with a note saying how much was dropped when the input
/// was longer.
fn read_tail(mut input: impl Read, limit: usize) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut dropped = 0usize;
    let mut chunk = [0u8; 8192];
    loop {
        let count = match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        kept.extend_from_slice(&chunk[..count]);
        // Compact only at twice the limit, so each byte is moved O(1) times.
        if kept.len() > limit.saturating_mul(2) {
            let excess = kept.len() - limit;
            kept.drain(..excess);
            dropped = dropped.saturating_add(excess);
        }
    }
    if kept.len() > limit {
        let excess = kept.len() - limit;
        kept.drain(..excess);
        dropped = dropped.saturating_add(excess);
    }
    let text = String::from_utf8_lossy(&kept);
    if dropped == 0 {
        return text.into_owned();
    }
    // Start at a whole line so the first one isn't a fragment.
    let start = text.find('\n').map_or(0, |newline| newline + 1);
    format!(
        "[{} earlier bytes of worker output dropped]\n{}",
        dropped + start,
        &text[start..]
    )
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: do not flash a console window for the worker.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::discovery::PluginFormat;
    use crate::host::pipl::PiPL;
    use std::collections::BTreeMap;

    fn info(archs: &[&str]) -> PluginInfo {
        let entry_points = archs
            .iter()
            .map(|a| (a.to_string(), "Main".to_string()))
            .collect();
        PluginInfo {
            path: "/plugins/Test.8bf".into(),
            binary: "/plugins/Test.8bf".into(),
            format: PluginFormat::Pe,
            archs: archs.iter().map(|a| a.to_string()).collect(),
            pipl: PiPL {
                name: "Test".into(),
                entry_points,
                ..PiPL::default()
            },
            file_size: 0,
            file_mtime: 0.0,
            error: None,
            extra: BTreeMap::new(),
        }
    }

    fn env(platform: Platform, arch: &str) -> Environment {
        Environment {
            platform,
            host_arch: arch.into(),
            self_exe: PathBuf::from("/nonexistent/bin/8bf-host"),
        }
    }

    fn config() -> Config {
        Config {
            plugin_dirs: Vec::new(),
            ..Config::default()
        }
    }

    fn command_of(plan: &Plan) -> Vec<String> {
        match &plan.placement {
            Placement::Worker { command, .. } => command
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect(),
            Placement::InProcess => Vec::new(),
        }
    }

    #[test]
    fn matching_filters_run_in_process() {
        let plan = plan(
            &info(&["win-x64"]),
            &config(),
            &env(Platform::Windows, "win-x64"),
        )
        .unwrap();
        assert_eq!(plan.placement, Placement::InProcess);
    }

    #[test]
    fn native_plugins_run_locally_on_any_platform() {
        let host = env(Platform::MacOs, "mac-arm64");
        let plan = plan(&info(&["native"]), &config(), &host).unwrap();
        assert_eq!(
            (plan.arch.as_str(), &plan.placement),
            ("native", &Placement::InProcess)
        );
    }

    #[test]
    fn worker_mode_isolates_matching_filters_in_this_binary() {
        let mut config = config();
        config.run_mode = RunMode::Worker;
        let plan = plan(&info(&["native"]), &config, &env(Platform::Unix, "native")).unwrap();
        assert_eq!(command_of(&plan), ["/nonexistent/bin/8bf-host"]);
    }

    #[test]
    fn windows_32_bit_filters_need_a_32_bit_worker() {
        let filter = info(&["win-x86"]);
        let host = env(Platform::Windows, "win-x64");
        let error = plan(&filter, &config(), &host).unwrap_err();
        assert!(error.to_string().contains(WORKER_WIN_X86));
        let mut config = config();
        config.workers.win_x86 = Some(PathBuf::from(r"C:\w\8bf-host-x86.exe"));
        let plan = plan(&filter, &config, &host).unwrap();
        assert_eq!(plan.arch, "win-x86");
        assert_eq!(command_of(&plan), [r"C:\w\8bf-host-x86.exe"]);
    }

    #[test]
    fn intel_filters_on_apple_silicon_use_rosetta() {
        let plan = plan(
            &info(&["mac-x64"]),
            &config(),
            &env(Platform::MacOs, "mac-arm64"),
        )
        .unwrap();
        assert_eq!(
            command_of(&plan),
            ["arch", "-x86_64", "/nonexistent/bin/8bf-host"]
        );
    }

    #[test]
    fn universal_filters_run_natively_on_a_mac() {
        let filter = info(&["mac-x64", "mac-arm64"]);
        let plan = plan(&filter, &config(), &env(Platform::MacOs, "mac-arm64")).unwrap();
        assert_eq!(plan.placement, Placement::InProcess);
    }

    // Wine paths are only built on Unix; on Windows, `/opt/...` resolves
    // against the current drive.
    #[cfg(unix)]
    #[test]
    fn windows_filters_on_linux_use_wine() {
        let filter = info(&["win-x64"]);
        let host = env(Platform::Unix, "native");
        let error = plan(&filter, &config(), &host).unwrap_err();
        assert!(error.to_string().contains("Wine"));
        assert_eq!(error.exit_code(), crate::error::exit::CANNOT_RUN);

        let mut config = config();
        config.workers.win_x64 = Some(PathBuf::from("/opt/w/8bf-host.exe"));
        config.wine.prefix = Some(PathBuf::from("/home/me/.wine-8bf"));
        let plan = plan(&filter, &config, &host).unwrap();
        assert_eq!(command_of(&plan), ["wine", r"Z:\opt\w\8bf-host.exe"]);
        let Placement::Worker { env, wine, .. } = &plan.placement else {
            panic!("expected a worker");
        };
        assert!(wine);
        assert!(env.contains(&("WINEPREFIX".into(), "/home/me/.wine-8bf".into())));
        assert!(env.contains(&("WINEDEBUG".into(), "-all".into())));
    }

    #[test]
    fn forced_in_process_fails_for_foreign_filters() {
        let mut config = config();
        config.run_mode = RunMode::InProcess;
        let error = plan(&info(&["win-x64"]), &config, &env(Platform::Unix, "native")).unwrap_err();
        assert!(error.to_string().contains("not built for native"));
    }

    #[test]
    fn filters_without_runnable_code_are_explained() {
        let mut filter = info(&["win-x64"]);
        filter.pipl.entry_points.clear();
        let error = plan(&filter, &config(), &env(Platform::Windows, "win-x64")).unwrap_err();
        assert!(error.to_string().contains("entry points: none"));
    }

    #[cfg(unix)]
    #[test]
    fn wine_paths_use_the_z_drive() {
        assert_eq!(wine_path(Path::new("/a/b.8bf")), r"Z:\a\b.8bf");
        assert!(looks_windows(Path::new(r"C:\x")));
        assert!(!looks_windows(Path::new("/x")));
    }

    #[test]
    fn long_progress_lines_are_skipped() {
        let input = format!(
            "PROGRESS 1 2\n{}PROGRESS 9 9\nPROGRESS 2 2",
            "x".repeat(1000)
        );
        let mut lines = Vec::new();
        for_each_short_line(input.as_bytes(), 32, |line| {
            lines.push(line.trim().to_string())
        });
        assert_eq!(lines, ["PROGRESS 1 2", "PROGRESS 2 2"]);
    }

    #[test]
    fn only_the_end_of_stderr_is_kept() {
        assert_eq!(read_tail(&b"short\n"[..], 100), "short\n");
        let input: String = (0..100).map(|n| format!("line {n}\n")).collect();
        let tail = read_tail(input.as_bytes(), 40);
        assert!(tail.starts_with('['), "{tail}");
        assert!(tail.ends_with("line 99\n"), "{tail}");
        assert!(tail.len() < 100);
        assert!(!tail.contains("line 1\n"));
    }

    #[test]
    fn a_missing_worker_program_is_reported() {
        let plan = Plan {
            arch: "native".into(),
            placement: Placement::Worker {
                command: vec!["/nonexistent/8bf-host-worker".into()],
                env: Vec::new(),
                wine: false,
            },
            description: String::new(),
        };
        let image = crate::host::image::PlaneImage::from_data(1, 1, 3, 8, vec![0; 3]).unwrap();
        let request = FilterRequest::new(image, 3, false);
        let result = run_worker(
            &plan,
            &info(&["native"]),
            &request,
            Hooks::default(),
            Action::Run,
            None,
        );
        assert_eq!(result.status, RunStatus::Error);
        assert!(result.message.contains("Could not start"));
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_held_open_after_exit_does_not_hang_the_run() {
        // The worker exits at once, but a process it started keeps its
        // stdout and stderr open for much longer than the grace period.
        let plan = Plan {
            arch: "native".into(),
            placement: Placement::Worker {
                command: vec!["/bin/sh".into(), "-c".into(), "sleep 30 & exit 3".into()],
                env: Vec::new(),
                wine: false,
            },
            description: String::new(),
        };
        let image = crate::host::image::PlaneImage::from_data(1, 1, 3, 8, vec![0; 3]).unwrap();
        let request = FilterRequest::new(image, 3, false);
        let started = Instant::now();
        let result = run_worker(
            &plan,
            &info(&["native"]),
            &request,
            Hooks::default(),
            Action::Run,
            None,
        );
        assert!(
            started.elapsed() < PIPE_GRACE * 4,
            "{:?}",
            started.elapsed()
        );
        assert_eq!(result.status, RunStatus::Error);
        assert!(result.message.contains("code 3"), "{}", result.message);
    }
}
