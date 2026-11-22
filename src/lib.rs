//! Delay Bridge: add, change or remove stream delay while live.

pub mod amf0;
pub mod app;
pub mod config;
pub mod engine;
pub mod media;
pub mod obs;
pub mod output;
pub mod plugin_link;
pub mod rtmp;
pub mod stretch;
pub mod web;

/// Log sink shared across tasks.
pub type Logger = std::sync::Arc<dyn Fn(String) + Send + Sync>;
