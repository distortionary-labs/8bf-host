//! One module per subcommand, plus the pieces they share.

pub mod about;
pub mod doctor;
pub mod list;
pub mod run;
pub mod serve;
pub mod show;
pub mod worker;

use serde::Serialize;

use crate::catalog::{filter_id, Catalog};
use crate::config::{cache_path, Config};
use crate::host::constants as c;
use crate::host::discovery::PluginInfo;
use crate::host::session::{FilterHost, FilterRequest, FilterResult, Hooks};
use crate::protocol::Action;
use crate::runner::{self, Environment, Placement, Plan};

/// Scans the configured folders, using the cache unless it is disabled.
pub fn load_catalog(config: &Config) -> Catalog {
    let cache = if config.cache { cache_path() } else { None };
    Catalog::scan(&config.plugin_dirs, cache.as_deref())
}

/// Which image modes a filter accepts, by name.
pub fn supported_modes(info: &PluginInfo) -> Vec<&'static str> {
    [
        (c::MODE_RGB, "rgb8"),
        (c::MODE_RGB48, "rgb16"),
        (c::MODE_GRAYSCALE, "gray8"),
        (c::MODE_GRAY16, "gray16"),
    ]
    .into_iter()
    .filter(|(mode, _)| info.pipl.supports_mode(*mode))
    .map(|(_, name)| name)
    .collect()
}

/// A filter as shown by `list`, `show` and the HTTP API.
#[derive(Clone, Debug, Serialize)]
pub struct FilterSummary {
    pub id: String,
    pub name: String,
    pub category: String,
    pub path: String,
    pub format: crate::host::discovery::PluginFormat,
    pub archs: Vec<String>,
    pub modes: Vec<&'static str>,
    pub has_about_box: bool,
    /// Where the filter would run, or null when it cannot run here.
    pub runs: Option<String>,
    /// Why the filter cannot run here.
    pub problem: Option<String>,
}

impl FilterSummary {
    pub fn new(info: &PluginInfo, config: &Config, env: &Environment) -> Self {
        Self::with_plan(info, runner::plan(info, config, env))
    }

    /// A summary for a plan the caller already made (or failed to make).
    pub fn with_plan(info: &PluginInfo, plan: crate::error::Result<Plan>) -> Self {
        let (runs, problem) = match plan {
            Ok(plan) => (Some(plan.description), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            id: filter_id(&info.key()),
            name: info.name(),
            category: info.category(),
            path: info.path.clone(),
            format: info.format,
            archs: info.archs.clone(),
            modes: supported_modes(info),
            has_about_box: !info.pipl.no_about_box,
            runs,
            problem,
        }
    }
}

/// Runs a filter where `plan` says, in this process or a worker.
pub fn execute(
    plan: &Plan,
    info: &PluginInfo,
    request: FilterRequest,
    hooks: Hooks,
    host: &mut FilterHost,
) -> FilterResult {
    match plan.placement {
        Placement::InProcess => host.run(info, request, hooks, Some(&plan.arch)),
        Placement::Worker { .. } => {
            runner::run_worker(plan, info, &request, hooks, Action::Run, None)
        }
    }
}
