//! `8bf-host doctor`: what this system can run, and why not.

use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use super::load_catalog;
use crate::cli::GlobalArgs;
use crate::config::{default_config_path, Config};
use crate::error::{Error, Result};
use crate::host::structs::uses_pack4;
use crate::runner::{self, Environment, Platform, PrefixArch};

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Print JSON instead of a report
    #[arg(long)]
    pub json: bool,
}

/// How a check turned out.
#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    /// Something optional is missing; only some filters are affected.
    Note,
    Fail,
}

#[derive(Serialize)]
struct Check {
    name: String,
    status: Status,
    detail: String,
}

fn check(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: if ok { Status::Ok } else { Status::Fail },
        detail: detail.into(),
    }
}

fn note(name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status: Status::Note,
        detail: detail.into(),
    }
}

/// Finds a program on PATH (or accepts an existing path).
fn find_program(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn worker_check(config: &Config, env: &Environment, arch: &str) -> Check {
    let (configured, sibling) = match arch {
        "win-x64" => (&config.workers.win_x64, runner::WORKER_WIN_X64),
        "win-x86" => (&config.workers.win_x86, runner::WORKER_WIN_X86),
        _ => (&config.workers.mac_x64, runner::WORKER_MAC_X64),
    };
    let path = configured
        .clone()
        .or_else(|| env.self_exe.parent().map(|dir| dir.join(sibling)));
    match path {
        Some(path) if path.is_file() => {
            check(format!("{arch} worker"), true, path.display().to_string())
        }
        Some(path) => note(
            format!("{arch} worker"),
            format!("not found ({}); {arch} filters cannot run", path.display()),
        ),
        None => note(format!("{arch} worker"), "not configured"),
    }
}

fn prefix_check(config: &Config) -> Check {
    let Some(prefix) = config.wine.effective_prefix() else {
        return note(
            "wine prefix",
            "no data folder; Wine uses its default prefix",
        );
    };
    match runner::wine_prefix_arch(&prefix) {
        Some(PrefixArch::Win64) => check(
            "wine prefix",
            true,
            format!("{} (64-bit)", prefix.display()),
        ),
        Some(PrefixArch::Win32) => check(
            "wine prefix",
            false,
            format!(
                "{} is 32-bit; 64-bit Windows filters cannot run{}",
                prefix.display(),
                if config.wine.prefix.is_some() {
                    " (remove wine.prefix to use 8bf-host's own)"
                } else {
                    ""
                }
            ),
        ),
        None if prefix.join("system.reg").exists() => {
            check("wine prefix", true, prefix.display().to_string())
        }
        None => check(
            "wine prefix",
            true,
            format!(
                "{} (created on the first Windows filter run)",
                prefix.display()
            ),
        ),
    }
}

pub fn run(global: &GlobalArgs, args: &DoctorArgs) -> Result<()> {
    let config = global.load_config()?;
    let env = Environment::current();
    let mut checks = Vec::new();

    let config_path = global.config.clone().or_else(default_config_path);
    checks.push(match &config_path {
        Some(path) if path.exists() => check("config file", true, path.display().to_string()),
        Some(path) => check(
            "config file",
            true,
            format!("{} (not present; using defaults)", path.display()),
        ),
        None => check("config file", true, "no config directory; using defaults"),
    });
    checks.push(check(
        "host",
        true,
        format!(
            "{} {} ({} FilterRecord layout)",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            if uses_pack4() {
                "Windows pack(4)"
            } else {
                "natural"
            }
        ) + &format!(", runs {} filters in-process", env.host_arch),
    ));
    if config.plugin_dirs.is_empty() {
        checks.push(check(
            "plug-in folders",
            false,
            "none configured; use --plugin-dir or plugin_dirs",
        ));
    }
    for dir in &config.plugin_dirs {
        checks.push(check(
            "plug-in folder",
            dir.is_dir(),
            dir.display().to_string(),
        ));
    }
    let catalog = load_catalog(&config);
    let runnable = catalog
        .filters
        .iter()
        .filter(|info| runner::plan(info, &config, &env).is_ok())
        .count();
    checks.push(check(
        "filters",
        !catalog.filters.is_empty(),
        format!("{} found, {runnable} can run here", catalog.filters.len()),
    ));
    for problem in &catalog.problems {
        checks.push(check(
            "unreadable plug-in",
            false,
            format!("{}: {}", problem.path.display(), problem.message),
        ));
    }
    match env.platform {
        Platform::Unix => {
            checks.push(match find_program(&config.wine.command) {
                Some(path) => check("wine", true, path.display().to_string()),
                None => check(
                    "wine",
                    false,
                    format!(
                        "'{}' not found; Windows filters cannot run",
                        config.wine.command
                    ),
                ),
            });
            checks.push(worker_check(&config, &env, "win-x64"));
            checks.push(worker_check(&config, &env, "win-x86"));
            checks.push(prefix_check(&config));
        }
        Platform::Windows => checks.push(worker_check(
            &config,
            &env,
            if env.host_arch == "win-x86" {
                "win-x64"
            } else {
                "win-x86"
            },
        )),
        Platform::MacOs => {
            if env.host_arch == "mac-arm64" {
                checks.push(worker_check(&config, &env, "mac-x64"));
            }
        }
    }
    let token = config.server.token.is_some();
    let loopback = config.server.listen.ip().is_loopback();
    checks.push(check(
        "server",
        loopback || token,
        format!(
            "listens on {}, {}, {}",
            config.server.listen,
            if token { "token required" } else { "no token" },
            if config.server.isolate {
                "filters isolated in workers"
            } else {
                "filters run in the server process"
            }
        ),
    ));

    if args.json {
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "checks": checks,
            "config": config,
        }))
        .map_err(|error| Error::Usage(error.to_string()))?;
        println!("{text}");
        return Ok(());
    }
    for item in &checks {
        let label = match item.status {
            Status::Ok => "ok  ",
            Status::Note => "note",
            Status::Fail => "FAIL",
        };
        println!("{label} {:<18} {}", item.name, item.detail);
    }
    if global.verbose {
        println!("\nEffective configuration:\n\n{}", config.to_toml());
    }
    Ok(())
}
