//! `8bf-host show`: details about one filter.

use clap::Args;

use super::{load_catalog, FilterSummary};
use crate::cli::GlobalArgs;
use crate::error::{Error, Result};
use crate::runner::Environment;

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Filter id, name, Category/Name, or plug-in file
    pub filter: String,

    /// Print JSON, including the raw PiPL
    #[arg(long)]
    pub json: bool,
}

pub fn run(global: &GlobalArgs, args: &ShowArgs) -> Result<()> {
    let config = global.load_config()?;
    let info = load_catalog(&config).resolve(&args.filter)?;
    let summary = FilterSummary::new(&info, &config, &Environment::current());
    if args.json {
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "filter": summary,
            "pipl": info.pipl,
            "binary": info.binary,
        }))
        .map_err(|error| Error::Usage(error.to_string()))?;
        println!("{text}");
        return Ok(());
    }
    println!("{}", summary.name);
    println!("  id:         {}", summary.id);
    println!("  category:   {}", summary.category);
    println!("  file:       {}", summary.path);
    println!("  code:       {}", summary.archs.join(", "));
    println!("  modes:      {}", summary.modes.join(", "));
    println!(
        "  about box:  {}",
        if summary.has_about_box { "yes" } else { "no" }
    );
    match (&summary.runs, &summary.problem) {
        (Some(runs), _) => println!("  runs:       {runs}"),
        (None, Some(problem)) => println!("  runs:       no. {problem}"),
        (None, None) => {}
    }
    Ok(())
}
