//! Run Photoshop-compatible filter plug-ins (`.8bf` files) outside Photoshop.
//!
//! The [`host`] module is the filter host itself: it loads a filter and
//! runs it on an image held in memory. The other modules build the
//! command line and HTTP server on top of it: [`catalog`] finds filters,
//! [`runner`] decides where they run, [`protocol`] talks to worker
//! processes, and [`imageio`] reads and writes image files.

pub mod catalog;
pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod host;
pub mod imageio;
pub mod preset;
pub mod protocol;
pub mod runner;
pub mod server;
