//! `8bf-host serve`: the HTTP API.

use std::net::SocketAddr;

use clap::Args;

use crate::cli::GlobalArgs;
use crate::error::Result;

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on [default: 127.0.0.1:8780]
    #[arg(short, long, value_name = "ADDR")]
    pub listen: Option<SocketAddr>,

    /// How many filters may run at once
    #[arg(short, long, value_name = "N")]
    pub jobs: Option<usize>,

    /// Run filters in the server process instead of worker processes (faster; a crashing filter stops the server)
    #[arg(long)]
    pub no_isolate: bool,

    /// Allow listening on a non-loopback address without a token
    #[arg(long)]
    pub insecure_no_auth: bool,
}

pub fn run(global: &GlobalArgs, args: &ServeArgs) -> Result<()> {
    let mut config = global.load_config()?;
    if let Some(listen) = args.listen {
        config.server.listen = listen;
    }
    if let Some(jobs) = args.jobs {
        config.server.jobs = jobs.max(1);
    }
    if args.no_isolate {
        config.server.isolate = false;
    }
    crate::server::serve(config, args.insecure_no_auth, global.quiet)
}
