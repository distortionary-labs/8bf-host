//! `8bf-host about`: show a filter's About box.

use clap::Args;

use super::load_catalog;
use crate::cli::GlobalArgs;
use crate::error::{Error, Result};
use crate::host::image::PlaneImage;
use crate::host::session::{FilterHost, FilterRequest, Hooks, RunStatus};
use crate::protocol::Action;
use crate::runner::{self, Environment, Placement};

#[derive(Debug, Args)]
pub struct AboutArgs {
    /// Filter id, name, Category/Name, or plug-in file
    pub filter: String,
}

pub fn run(global: &GlobalArgs, args: &AboutArgs) -> Result<()> {
    let config = global.load_config()?;
    let info = load_catalog(&config).resolve(&args.filter)?;
    if info.pipl.no_about_box {
        return Err(Error::Usage(format!("{} has no About box", info.name())));
    }
    let plan = runner::plan(&info, &config, &Environment::current())?;
    let verbose = global.verbose;
    let log = move |line: &str| {
        if verbose {
            eprintln!("[host] {line}");
        }
    };
    let result = match plan.placement {
        Placement::InProcess => {
            crate::host::macapp::ensure_application();
            FilterHost::new().about(&info, 0, Box::new(log), Some(&plan.arch))
        }
        Placement::Worker { .. } => {
            let placeholder =
                PlaneImage::new(1, 1, 3, 8).map_err(|error| Error::CannotRun(error.to_string()))?;
            let request = FilterRequest::new(placeholder, 3, false);
            let hooks = Hooks {
                log: Box::new(log),
                ..Hooks::default()
            };
            runner::run_worker(&plan, &info, &request, hooks, Action::About, None)
        }
    };
    match result.status {
        RunStatus::Error => Err(Error::FilterFailed {
            message: result.message,
            code: result.code,
        }),
        _ => Ok(()),
    }
}
