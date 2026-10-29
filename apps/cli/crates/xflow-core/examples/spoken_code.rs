//! Line-oriented local formatter for the opt-in WAV benchmark. No credentials.
use std::io::{self, BufRead};
use xflow_core::{config::Config, text::before_cleanup};

fn main() -> io::Result<()> {
    let config = Config::default();
    for line in io::stdin().lock().lines() {
        println!("{}", before_cleanup(&line?, &config, false));
    }
    Ok(())
}
