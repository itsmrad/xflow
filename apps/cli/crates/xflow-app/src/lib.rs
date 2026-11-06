pub mod config_edit;
pub mod context;
#[cfg(unix)]
pub mod daemon;
pub mod gsettings;
pub mod local_models;
#[cfg(target_os = "macos")]
mod macos_settings;
pub mod mcp;
pub mod meeting;
#[cfg(target_os = "macos")]
pub mod overlay_pill;
pub mod paths;
#[cfg(unix)]
mod retry;
mod speech;
pub mod store;
#[cfg(all(unix, feature = "test-support"))]
mod test_support;
#[cfg(unix)]
pub mod transport;
#[cfg(unix)]
pub mod tui;
