//! Native integrations kept outside the daemon's state machine.
#[cfg(feature = "native-audio")]
mod audio;
#[cfg(target_os = "linux")]
mod bridge;
mod desktop;
#[cfg(target_os = "macos")]
mod macos;
pub mod notifications;
pub mod sounds;

#[cfg(feature = "native-audio")]
pub use audio::{input_devices, CpalCapture, InputDevice};
#[cfg(target_os = "linux")]
pub use bridge::run_desktop_bridge;
pub use desktop::LinuxDesktop;
#[cfg(target_os = "linux")]
pub use desktop::LinuxDesktop as NativeDesktop;
#[cfg(target_os = "macos")]
pub use macos::{microphone_permission, permissions, MacDesktop, MacDesktop as NativeDesktop};

pub use notifications::notify;
pub use sounds::{play, Cue};

/// macOS has no desktop shell bridge; the CLI socket is the control surface.
#[cfg(target_os = "macos")]
pub async fn run_desktop_bridge(
    _events: tokio::sync::broadcast::Receiver<xflow_core::DesktopEvent>,
    _commands: tokio::sync::mpsc::WeakSender<xflow_core::ipc::Command>,
) -> anyhow::Result<()> {
    Ok(())
}
