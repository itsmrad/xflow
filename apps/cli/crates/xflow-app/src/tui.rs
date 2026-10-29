//! Keyboard-first control center. Blocking desktop/config work lives off the UI task.
mod integrations;
mod options;
mod render;
mod runtime;
mod state;
mod theme;

pub use integrations::{smart_formatting, smart_line};
pub use options::key_label;
pub use runtime::run;
pub use state::Step;

#[cfg(test)]
mod tests;
