use anyhow::{bail, Context, Result};
use std::{collections::HashMap, time::Duration};

/// Send a native desktop notification; the daemon controls when it is enabled.
pub async fn notify(summary: &str, body: &str) -> Result<()> {
    if summary.len() > 4096 || body.len() > 64 * 1024 {
        bail!("notification text exceeds limit");
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await?;
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        let _: u32 = proxy
            .call(
                "Notify",
                &(
                    "XFlow",
                    0_u32,
                    "audio-input-microphone",
                    summary,
                    body,
                    Vec::<&str>::new(),
                    hints,
                    5000_i32,
                ),
            )
            .await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("desktop notification timed out")?
}
