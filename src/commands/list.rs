//! `8bf-host list`: the filters found in the plug-in folders.

use clap::Args;

use super::{load_catalog, FilterSummary};
use crate::cli::GlobalArgs;
use crate::error::{Error, Result};
use crate::runner::Environment;

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Print JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

pub fn run(global: &GlobalArgs, args: &ListArgs) -> Result<()> {
    let config = global.load_config()?;
    let catalog = load_catalog(&config);
    let env = Environment::current();
    let summaries: Vec<FilterSummary> = catalog
        .filters
        .iter()
        .map(|info| FilterSummary::new(info, &config, &env))
        .collect();
    if args.json {
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "filters": summaries,
            "problems": catalog.problems,
        }))
        .map_err(|error| Error::Usage(error.to_string()))?;
        println!("{text}");
        return Ok(());
    }
    if !global.quiet {
        for problem in &catalog.problems {
            eprintln!("warning: {}: {}", problem.path.display(), problem.message);
        }
    }
    if summaries.is_empty() {
        if !global.quiet {
            eprintln!(
                "No filters found in {}. Add folders with --plugin-dir, \
                 EIGHTBF_HOST_PLUGIN_PATH or plugin_dirs in the config file.",
                if config.plugin_dirs.is_empty() {
                    "any folder".to_string()
                } else {
                    config
                        .plugin_dirs
                        .iter()
                        .map(|dir| dir.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
        }
        return Ok(());
    }
    let width = summaries
        .iter()
        .map(|s| s.category.chars().count() + s.name.chars().count() + 1)
        .max()
        .unwrap_or(0);
    for summary in &summaries {
        let label = format!("{}/{}", summary.category, summary.name);
        let status = match (&summary.runs, &summary.problem) {
            (Some(runs), _) => runs.clone(),
            (None, Some(_)) => format!("cannot run here ({})", summary.archs.join(", ")),
            (None, None) => String::new(),
        };
        println!("{}  {label:<width$}  {status}", summary.id);
    }
    Ok(())
}
