//! The filter host: everything needed to load and run a Photoshop filter.
//!
//! This module has no knowledge of files, configuration or the network. It
//! takes an image in memory, runs a filter on it and returns the result.
//! The same code runs in the main process, in a worker process, and in a
//! Windows build of the binary under Wine.

pub mod colors;
pub mod constants;
pub mod context;
pub mod descriptors;
pub mod discovery;
pub mod display;
pub mod image;
pub mod loader;
pub mod macapp;
pub mod masking;
pub mod memory;
pub mod pe;
pub mod pipl;
pub mod pixels;
pub mod rsrc;
pub mod session;
pub mod structs;
