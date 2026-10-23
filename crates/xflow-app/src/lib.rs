pub mod config_edit;
#[cfg(unix)]
pub mod daemon;
pub mod gsettings;
#[cfg(target_os = "macos")]
mod macos_settings;
#[cfg(target_os = "macos")]
pub mod overlay_pill;
pub mod paths;
mod speech;
pub mod store;
#[cfg(all(unix, feature = "test-support"))]
mod test_support;
#[cfg(unix)]
pub mod transport;
#[cfg(unix)]
pub mod tui;
