//! Prints the production `hints::select` for benchmark contexts
//! (benchmarks/devterms/terms.py). One JSON object per stdin line:
//! `{"app": "com.apple.Terminal", "terminal": true, "recent": "earlier dictation text"}`
//! -> one JSON array of shipped terms, best first, for the default config.
use std::io::BufRead;
use xflow_core::hints::{mentions, select, Signals};

fn main() {
    let config = xflow_core::config::Config::default();
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let context: serde_json::Value = serde_json::from_str(&line).expect("JSON context");
        let recent = mentions(context["recent"].as_str().unwrap_or_default());
        let picked = select(
            &config,
            &Signals {
                terminal: context["terminal"].as_bool().unwrap_or_default(),
                app: context["app"].as_str(),
                title: &[],
                recent: &recent,
            },
        );
        println!("{}", serde_json::to_string(&picked).expect("JSON"));
    }
}
