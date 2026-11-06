//! Meeting detection. macOS reads Core Audio process objects and window titles;
//! other platforms report it as unsupported.
pub use xflow_core::meeting_detect::{Decision, Event};

#[cfg(target_os = "macos")]
pub use crate::macos::meeting::{observe, Observer};

#[cfg(not(target_os = "macos"))]
pub struct Observer;
#[cfg(not(target_os = "macos"))]
pub fn observe(_: impl FnMut(Event) + Send + 'static) -> anyhow::Result<Observer> {
    anyhow::bail!("meeting detection is unsupported on this platform")
}
