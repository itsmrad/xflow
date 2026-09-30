//! Native integrations kept outside the daemon's state machine.
#[cfg(feature = "native-audio")]
mod audio;
mod bridge;
mod desktop;

#[cfg(feature = "native-audio")]
pub use audio::CpalCapture;
pub use bridge::run_desktop_bridge;
pub use desktop::LinuxDesktop;
