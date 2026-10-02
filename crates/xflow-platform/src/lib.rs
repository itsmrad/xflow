//! Native integrations kept outside the daemon's state machine.
#[cfg(feature = "native-audio")]
mod audio;
mod bridge;
mod desktop;
pub mod notifications;
pub mod sounds;

#[cfg(feature = "native-audio")]
pub use audio::{input_devices, CpalCapture, InputDevice};
pub use bridge::run_desktop_bridge;
pub use desktop::LinuxDesktop;

pub use notifications::notify;
pub use sounds::{play, Cue};
