//! Field-observer acceptance harness: pins the focused field and prints each
//! notification and read for `secs` seconds. Prints only lengths and whether the
//! read contains `marker`, never field text.
use std::time::{Duration, Instant};
use xflow_core::{config::InjectionConfig, Desktop, FieldEvent};
use xflow_platform::NativeDesktop;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let marker = args.next().expect("usage: observe <marker> [secs]");
    let secs: u64 = args.next().map_or(Ok(10), |s| s.parse())?;
    let desktop = NativeDesktop::new(&InjectionConfig::default());
    let started = Instant::now();
    let target = desktop.context().await?;
    let Some(mut watch) = desktop.watch_field(&target).await else {
        println!("app={:?} watch=none", target.app_id);
        return Ok(());
    };
    println!(
        "app={:?} watch_ms={}",
        target.app_id,
        started.elapsed().as_millis()
    );
    let report = |label: &str, field: Option<xflow_core::FieldText>, at: Instant| match field {
        Some(f) => println!(
            "{label} t={}ms read_ms={} len={} caret={:?} marker={}",
            started.elapsed().as_millis(),
            at.elapsed().as_millis(),
            f.value.encode_utf16().count(),
            f.caret,
            f.value.contains(&marker)
        ),
        None => println!("{label} t={}ms unreadable", started.elapsed().as_millis()),
    };
    let at = Instant::now();
    report("baseline", watch.read().await, at);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            event = watch.next() => match event {
                Some(FieldEvent::Changed) => { let at = Instant::now(); report("changed", watch.read().await, at) }
                Some(FieldEvent::Focus) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    println!("focus t={}ms still_focused={}", started.elapsed().as_millis(), watch.focused().await);
                }
                None => { println!("ended"); break }
            }
        }
    }
    let at = Instant::now();
    report("final", watch.read().await, at);
    Ok(())
}
