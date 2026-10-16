//! Command-line definitions and dispatch.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::commands;
use crate::config::{Config, RunMode};
use crate::error::{exit, Error, Result};

const AFTER_HELP: &str = "\
Examples:
  8bf-host list
  8bf-host run \"Acme/Unsharp\" photo.png sharp.png --dialog --save-preset unsharp.json
  8bf-host run \"Acme/Unsharp\" photo.png sharp.png --preset unsharp.json
  cat in.png | 8bf-host run 1a2b3c4d5e6f - - --format png > out.png
  8bf-host serve --listen 127.0.0.1:8780

Exit codes: 0 success, 1 filter failed, 2 usage, 3 filter not found,
4 cannot run here, 5 image read/write, 6 config, 130 cancelled.";

/// Run Photoshop-compatible filter plug-ins (.8bf) from the command line or over HTTP.
#[derive(Debug, Parser)]
#[command(name = "8bf-host", version, about, after_help = AFTER_HELP)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Configuration file [default: platform config dir/8bf-host/config.toml]
    #[arg(short, long, global = true, value_name = "FILE", env = crate::config::ENV_CONFIG)]
    pub config: Option<PathBuf>,

    /// Also look for filters in DIR (repeatable)
    #[arg(short = 'P', long = "plugin-dir", global = true, value_name = "DIR")]
    pub plugin_dirs: Vec<PathBuf>,

    /// Where filters run: auto, in-process or worker
    #[arg(long, global = true, value_name = "MODE")]
    pub run_mode: Option<RunMode>,

    /// Ignore the scan cache and read every plug-in file again
    #[arg(long, global = true)]
    pub rescan: bool,

    /// Print the host's log to stderr
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Print nothing but errors
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the filters found in the plug-in folders
    List(commands::list::ListArgs),
    /// Show details about one filter
    Show(commands::show::ShowArgs),
    /// Run a filter on an image file
    Run(Box<commands::run::RunArgs>),
    /// Show a filter's About box
    About(commands::about::AboutArgs),
    /// Serve the filters over HTTP
    Serve(commands::serve::ServeArgs),
    /// Check the configuration and what this system can run
    Doctor(commands::doctor::DoctorArgs),
    /// Run a job file (used internally by 8bf-host)
    #[command(hide = true)]
    Worker(commands::worker::WorkerArgs),
}

impl GlobalArgs {
    /// Loads the configuration and applies these flags on top.
    ///
    /// # Errors
    ///
    /// Fails when the configuration file is invalid.
    pub fn load_config(&self) -> Result<Config> {
        let mut config = Config::load(self.config.as_deref())?;
        for dir in &self.plugin_dirs {
            if !config.plugin_dirs.contains(dir) {
                config.plugin_dirs.push(dir.clone());
            }
        }
        if let Some(mode) = self.run_mode {
            config.run_mode = mode;
        }
        if self.rescan {
            config.cache = false;
        }
        Ok(config)
    }
}

/// Parses arguments, runs the command and returns the exit code.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let quiet = cli.global.quiet;
    let result = match cli.command {
        Command::List(args) => commands::list::run(&cli.global, &args),
        Command::Show(args) => commands::show::run(&cli.global, &args),
        Command::Run(args) => commands::run::run(&cli.global, &args),
        Command::About(args) => commands::about::run(&cli.global, &args),
        Command::Serve(args) => commands::serve::run(&cli.global, &args),
        Command::Doctor(args) => commands::doctor::run(&cli.global, &args),
        Command::Worker(args) => return commands::worker::run(&args),
    };
    match result {
        Ok(()) => ExitCode::from(exit::SUCCESS),
        Err(error) => {
            report(&error, quiet);
            ExitCode::from(error.exit_code())
        }
    }
}

fn report(error: &Error, quiet: bool) {
    if matches!(error, Error::Cancelled) {
        if !quiet {
            eprintln!("cancelled");
        }
        return;
    }
    eprintln!("error: {error}");
    let hint = match error {
        Error::FilterFailed { .. } => Some("run again with --verbose to see the host log"),
        Error::CannotRun(_) => Some("'8bf-host doctor' shows what this system can run"),
        _ => None,
    };
    if let (Some(hint), false) = (hint, quiet) {
        eprintln!("hint: {hint}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli =
            Cli::try_parse_from(["8bf-host", "list", "-P", "/x", "--run-mode", "worker"]).unwrap();
        assert_eq!(cli.global.plugin_dirs, [PathBuf::from("/x")]);
        assert_eq!(cli.global.run_mode, Some(RunMode::Worker));
    }

    #[test]
    fn verbose_and_quiet_conflict() {
        assert!(Cli::try_parse_from(["8bf-host", "list", "-v", "-q"]).is_err());
    }
}
