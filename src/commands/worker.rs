//! `8bf-host worker`: runs one job written by another 8bf-host process.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use crate::protocol;

#[derive(Debug, Args)]
pub struct WorkerArgs {
    /// The job's request.json
    pub request: PathBuf,
}

pub fn run(args: &WorkerArgs) -> ExitCode {
    let code = protocol::run_job(&args.request, Box::new(std::io::stdout()));
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
