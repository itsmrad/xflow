//! Keyboard-first control center. Blocking desktop/config work lives off the UI task.
mod ai_apps;
mod chat;
mod chat_view;
mod integrations;
mod meetings;
mod on_device;
mod options;
mod render;
mod runtime;
mod state;
mod theme;

pub use integrations::{smart_formatting, smart_line};
pub use options::key_label;
pub use runtime::run;
pub use state::Step;
pub use theme::{dumb_term, env_var, no_color};

#[cfg(test)]
mod chat_tests;
#[cfg(test)]
mod tests;
