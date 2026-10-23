//! Keyboard-first control center. Blocking desktop/config work lives off the UI task.
mod integrations;
mod options;
mod render;
mod runtime;
mod state;
mod theme;

pub use options::key_label;
pub use runtime::run;

#[cfg(test)]
mod tests;
