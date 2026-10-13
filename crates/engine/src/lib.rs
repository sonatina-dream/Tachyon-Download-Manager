//! Tachyon download engine.
//!
//! A clean-room implementation of the techniques Internet Download Manager is known
//! for: dynamic file segmentation over several HTTP connections, connection reuse,
//! resumable transfers that survive restarts, a download queue with a scheduler,
//! per-download and global speed limits, automatic file categories, and a local
//! endpoint for browser integration.

mod categories;
mod engine;
mod error;
mod fsutil;
mod http;
mod integration;
mod limiter;
mod persist;
mod segment;
mod task;
mod types;

pub use categories::{GENERAL, classify, default_categories, extension_of, looks_downloadable};
pub use engine::Engine;
pub use error::EngineError;
pub use fsutil::PART_SUFFIX;
pub use http::{derive_file_name, sanitize_file_name};
pub use types::*;
