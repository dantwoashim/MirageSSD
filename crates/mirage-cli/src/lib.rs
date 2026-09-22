//! MirageSSD command-line interface library: the command modules are shared
//! with `mirage-ui.exe`, which orchestrates first-run volume creation
//! in-process through the same code paths as the CLI.

#![deny(unsafe_code)]

#[allow(unsafe_code)]
pub mod client;
pub mod commands;
pub mod output;
