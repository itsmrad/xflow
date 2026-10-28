//! Explicit desktop acceptance harness: no microphone, provider, or daemon configuration.
use xflow_core::{config::InjectionConfig, Desktop};
use xflow_platform::NativeDesktop;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let text = std::env::args().nth(1).expect("usage: deliver <text>");
    let desktop = NativeDesktop::new(&InjectionConfig::default());
    let target = desktop.context().await?;
    let outcome = desktop.inject(&text, &target).await?;
    // Keep the runtime alive for adapters which restore the clipboard asynchronously.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let field = match desktop.watch_field(&target).await {
        Some(mut watch) => watch.read().await.map(|f| f.value),
        None => None,
    };
    println!(
        "outcome={outcome:?} field_contains={}",
        field.is_some_and(|s| s.contains(&text))
    );
    Ok(())
}
