#![allow(clippy::uninlined_format_args)]

pub mod app;
pub mod auth;
pub mod config;
pub mod data;
pub mod markdown;
pub mod media;
pub mod reddit;
pub mod release_notes;
pub mod session;
pub mod storage;
pub mod theme;
pub mod ui;
pub mod update;
pub mod video;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use app::run;
