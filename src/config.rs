//! Configuration: built-in defaults, a TOML file, environment variables.
//!
//! Command-line flags are applied on top by the CLI. The file lives at
//! `$XDG_CONFIG_HOME/8bf-host/config.toml` on Linux,
//! `~/Library/Application Support/8bf-host/config.toml` on macOS and
//! `%APPDATA%\8bf-host\config.toml` on Windows, or wherever
//! `EIGHTBF_HOST_CONFIG` points. A missing file is not an error.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Overrides the configuration file location.
pub const ENV_CONFIG: &str = "EIGHTBF_HOST_CONFIG";
/// Extra plug-in folders, separated like `PATH`.
pub const ENV_PLUGIN_PATH: &str = "EIGHTBF_HOST_PLUGIN_PATH";
/// The bearer token the server requires.
pub const ENV_TOKEN: &str = "EIGHTBF_HOST_TOKEN";

/// The directory name used under the platform's config and cache folders.
pub const APP_DIR: &str = "8bf-host";

/// Where filters run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunMode {
    /// In this process when the filter's architecture allows, else a worker.
    #[default]
    Auto,
    /// Always in this process; fail if the architecture does not match.
    InProcess,
    /// Always in a separate worker process, for crash isolation.
    Worker,
}

impl std::str::FromStr for RunMode {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "auto" => Ok(RunMode::Auto),
            "in-process" => Ok(RunMode::InProcess),
            "worker" => Ok(RunMode::Worker),
            other => Err(format!(
                "unknown run mode '{other}' (expected auto, in-process or worker)"
            )),
        }
    }
}

/// Builds of this program for other architectures, used as workers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Workers {
    /// A 64-bit Windows build (`x86_64-pc-windows-gnu`). On Linux it runs
    /// under Wine; on Windows it runs 64-bit filters from a 32-bit host.
    pub win_x64: Option<PathBuf>,
    /// A 32-bit Windows build (`i686-pc-windows-gnu`), for 32-bit filters.
    pub win_x86: Option<PathBuf>,
    /// An Intel macOS build, for Intel-only filters on Apple silicon. It
    /// runs under Rosetta. Not needed when this binary is universal.
    pub mac_x64: Option<PathBuf>,
}

/// How Windows workers are started on Linux and other Unix systems.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wine {
    /// The wine executable.
    pub command: String,
    /// The `WINEPREFIX` to use; empty for 8bf-host's own prefix (see
    /// [`default_wine_prefix`]), which it creates on first use.
    pub prefix: Option<PathBuf>,
}

impl Wine {
    /// The prefix Windows workers run in: the configured one, else
    /// [`default_wine_prefix`].
    pub fn effective_prefix(&self) -> Option<PathBuf> {
        self.prefix.clone().or_else(default_wine_prefix)
    }
}

impl Default for Wine {
    fn default() -> Self {
        Self {
            command: "wine".into(),
            prefix: None,
        }
    }
}

/// Settings for `8bf-host serve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    /// The address to listen on.
    pub listen: SocketAddr,
    /// A bearer token clients must send. Prefer `EIGHTBF_HOST_TOKEN` over
    /// writing it here.
    pub token: Option<String>,
    /// The largest request body accepted, in megabytes.
    pub max_upload_mb: u64,
    /// The largest image accepted, in pixels.
    pub max_pixels: u64,
    /// Filter runs taking longer than this are cancelled.
    pub timeout_secs: u64,
    /// How many filters may run at once.
    pub jobs: usize,
    /// Run every filter in a worker process so a crashing filter cannot
    /// take the server down.
    pub isolate: bool,
}

/// Default port: unassigned by IANA and easy to remember (8 + "bf" -> 8780).
pub const DEFAULT_PORT: u16 = 8780;

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)),
            token: None,
            max_upload_mb: 256,
            max_pixels: 100_000_000,
            timeout_secs: 300,
            jobs: 2,
            isolate: true,
        }
    }
}

/// The longest run timeout accepted: one day.
pub const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60;

impl Server {
    /// Checks the server settings and normalizes the token.
    ///
    /// A token with surrounding whitespace is trimmed, the way the
    /// environment variable is, and an empty one counts as no token, so it
    /// cannot satisfy the check that guards non-loopback addresses.
    ///
    /// # Errors
    ///
    /// Describes the first limit that would reject every request or overflow:
    /// a zero `jobs`, `max_upload_mb` or `max_pixels`, or a `timeout_secs`
    /// outside `1..=MAX_TIMEOUT_SECS`.
    pub fn validate(&mut self) -> Result<(), String> {
        if let Some(token) = &self.token {
            let trimmed = token.trim();
            self.token = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
        let positive = [
            ("server.jobs", self.jobs as u64),
            ("server.max_upload_mb", self.max_upload_mb),
            ("server.max_pixels", self.max_pixels),
        ];
        if let Some((name, _)) = positive.iter().find(|(_, value)| *value == 0) {
            return Err(format!("{name} must be at least 1"));
        }
        if !(1..=MAX_TIMEOUT_SECS).contains(&self.timeout_secs) {
            return Err(format!(
                "server.timeout_secs must be between 1 and {MAX_TIMEOUT_SECS}"
            ));
        }
        Ok(())
    }
}

/// The complete configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Folders scanned (with subfolders) for filters.
    pub plugin_dirs: Vec<PathBuf>,
    /// Where filters run.
    pub run_mode: RunMode,
    /// Run 8-bit-only filters on an 8-bit copy of 16-bit images.
    pub allow_8bit_fallback: bool,
    /// Cache filter scans between runs.
    pub cache: bool,
    pub workers: Workers,
    pub wine: Wine,
    pub server: Server,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            plugin_dirs: default_plugin_dirs(),
            run_mode: RunMode::Auto,
            allow_8bit_fallback: true,
            cache: true,
            workers: Workers::default(),
            wine: Wine::default(),
            server: Server::default(),
        }
    }
}

/// Common install locations for Photoshop plug-ins that exist on this system.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(windows) {
        let program_files = std::env::var_os("ProgramFiles")
            .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from);
        let adobe = program_files
            .join("Common Files")
            .join("Adobe")
            .join("Plug-Ins");
        candidates.push(adobe.join("CC"));
        candidates.push(adobe);
    } else if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from(
            "/Library/Application Support/Adobe/Plug-Ins/CC",
        ));
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Library/Application Support/Adobe/Plug-Ins/CC"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("8bf-plugins"));
    }
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

/// The configuration file path: `EIGHTBF_HOST_CONFIG`, or the platform default.
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(ENV_CONFIG) {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir().map(|dir| dir.join(APP_DIR).join("config.toml"))
}

/// The scan cache file.
pub fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join(APP_DIR).join("catalog.json"))
}

/// 8bf-host's own Wine prefix, used unless `wine.prefix` is set.
///
/// Wine's default, `~/.wine`, may be 32-bit or set up for other programs,
/// so 8bf-host keeps a prefix of its own under the local data folder.
pub fn default_wine_prefix() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join(APP_DIR).join("wine"))
}

/// Expands a leading `~` to the home directory.
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match dirs::home_dir() {
        Some(home) => home.join(rest),
        None => path.to_path_buf(),
    }
}

impl Config {
    /// Loads the configuration file (if any) and applies environment variables.
    ///
    /// `path` overrides the default location; an explicitly named file
    /// must exist.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or is not valid configuration.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let explicit = path.is_some();
        let path = path.map(Path::to_path_buf).or_else(default_config_path);
        let mut config = match path {
            Some(path) if explicit || path.exists() => Self::from_file(&path)?,
            _ => Self::default(),
        };
        config.apply_env(|name| std::env::var_os(name));
        Ok(config)
    }

    /// Reads one configuration file.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not parse.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| Error::Config {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        Self::from_toml(&text).map_err(|reason| Error::Config {
            path: path.to_path_buf(),
            reason,
        })
    }

    /// Parses configuration text.
    ///
    /// # Errors
    ///
    /// Returns the parser's message for invalid TOML or unknown keys.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let mut config: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        config.plugin_dirs = config.plugin_dirs.iter().map(|p| expand_home(p)).collect();
        for path in [
            &mut config.workers.win_x64,
            &mut config.workers.win_x86,
            &mut config.workers.mac_x64,
            &mut config.wine.prefix,
        ]
        .into_iter()
        .flatten()
        {
            *path = expand_home(path);
        }
        config.server.validate()?;
        Ok(config)
    }

    /// Applies environment variables, read through `var` so tests can fake them.
    pub fn apply_env(&mut self, var: impl Fn(&str) -> Option<std::ffi::OsString>) {
        if let Some(paths) = var(ENV_PLUGIN_PATH) {
            for path in std::env::split_paths(&paths) {
                if !path.as_os_str().is_empty() && !self.plugin_dirs.contains(&path) {
                    self.plugin_dirs.push(path);
                }
            }
        }
        if let Some(token) = var(ENV_TOKEN) {
            let token = token.to_string_lossy().trim().to_string();
            if !token.is_empty() {
                self.server.token = Some(token);
            }
        }
    }

    /// The configuration as TOML, for `8bf-host doctor`.
    pub fn to_toml(&self) -> String {
        let mut shown = self.clone();
        if shown.server.token.is_some() {
            shown.server.token = Some("(set)".into());
        }
        toml::to_string_pretty(&shown).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.run_mode, RunMode::Auto);
        assert!(config.server.isolate);
        assert_eq!(config.server.listen.port(), DEFAULT_PORT);
    }

    #[test]
    fn sections_are_read() {
        let config = Config::from_toml(
            r#"
            plugin_dirs = ["/opt/filters"]
            run_mode = "worker"
            [workers]
            win_x64 = "/opt/8bf-host.exe"
            [wine]
            prefix = "/tmp/prefix"
            [server]
            listen = "0.0.0.0:9000"
            jobs = 4
            "#,
        )
        .unwrap();
        assert_eq!(config.plugin_dirs, [PathBuf::from("/opt/filters")]);
        assert_eq!(config.run_mode, RunMode::Worker);
        assert_eq!(
            config.workers.win_x64,
            Some(PathBuf::from("/opt/8bf-host.exe"))
        );
        assert_eq!(config.wine.command, "wine");
        assert_eq!(config.server.listen.port(), 9000);
        assert_eq!(config.server.jobs, 4);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        assert!(Config::from_toml("plugin_dir = []").is_err());
        assert!(Config::from_toml("run_mode = \"sometimes\"").is_err());
        assert!(Config::from_toml("[server]\njobs = 0").is_err());
    }

    #[test]
    fn environment_adds_folders_and_token() {
        let mut config = Config::from_toml("plugin_dirs = [\"/a\"]").unwrap();
        let joined = std::env::join_paths(["/a", "/b"]).unwrap();
        config.apply_env(|name| match name {
            ENV_PLUGIN_PATH => Some(joined.clone()),
            ENV_TOKEN => Some(OsString::from(" secret \n")),
            _ => None,
        });
        assert_eq!(
            config.plugin_dirs,
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert_eq!(config.server.token.as_deref(), Some("secret"));
        assert!(config.to_toml().contains("(set)"));
    }

    #[test]
    fn tilde_expands_to_home() {
        let expanded = expand_home(Path::new("~/x"));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home.join("x"));
        }
        assert_eq!(expand_home(Path::new("/abs")), PathBuf::from("/abs"));
    }

    #[test]
    fn the_example_file_is_valid_and_matches_the_defaults() {
        let text = include_str!("../config.example.toml");
        let config = Config::from_toml(text).unwrap();
        let defaults = Config::default();
        assert_eq!(config.run_mode, defaults.run_mode);
        assert_eq!(config.server, defaults.server);
        assert_eq!(config.wine, defaults.wine);
        assert_eq!(config.workers, defaults.workers);
    }

    #[test]
    fn limits_that_reject_everything_are_errors() {
        for (setting, expected) in [
            ("jobs = 0", "server.jobs"),
            ("max_upload_mb = 0", "server.max_upload_mb"),
            ("max_pixels = 0", "server.max_pixels"),
            ("timeout_secs = 0", "server.timeout_secs"),
            ("timeout_secs = 9223372036854775807", "server.timeout_secs"),
        ] {
            let error = Config::from_toml(&format!("[server]\n{setting}\n")).unwrap_err();
            assert!(error.contains(expected), "{setting}: {error}");
        }
    }

    #[test]
    fn blank_tokens_are_no_token() {
        let config = Config::from_toml("[server]\ntoken = \"  \"\n").unwrap();
        assert_eq!(config.server.token, None);
        let config = Config::from_toml("[server]\ntoken = \" abc\\n\"\n").unwrap();
        assert_eq!(config.server.token.as_deref(), Some("abc"));
    }

    #[test]
    fn run_modes_parse() {
        assert_eq!("worker".parse::<RunMode>(), Ok(RunMode::Worker));
        assert!("bogus".parse::<RunMode>().is_err());
    }
}
