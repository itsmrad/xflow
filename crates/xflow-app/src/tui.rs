//! Keyboard-first control center. Blocking desktop/config work lives off the UI task.
mod integrations;
mod render;
mod runtime;
mod state;
mod theme;

pub use runtime::run;

#[cfg(test)]
mod tests;
