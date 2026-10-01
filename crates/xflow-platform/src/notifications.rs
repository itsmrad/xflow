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

#[cfg(test)]
mod tests {
    use super::*;
    struct StubNotifications;
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl StubNotifications {
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            app: &str,
            replaces: u32,
            icon: &str,
            summary: &str,
            body: &str,
            actions: Vec<String>,
            hints: HashMap<String, zbus::zvariant::OwnedValue>,
            expires: i32,
        ) -> u32 {
            assert_eq!(app, "XFlow");
            assert_eq!(replaces, 0);
            assert!(!icon.is_empty());
            assert_eq!(summary, "Test");
            assert_eq!(body, "Message");
            assert!(actions.is_empty() && hints.is_empty());
            assert_eq!(expires, 5000);
            42
        }
    }
    #[tokio::test]
    #[ignore = "requires an isolated dbus-run-session bus"]
    async fn notifications_use_native_bus_contract() {
        let service = zbus::ConnectionBuilder::session()
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", StubNotifications)
            .unwrap()
            .build()
            .await
            .unwrap();
        notify("Test", "Message").await.unwrap();
        assert!(notify(&"x".repeat(4097), "Message").await.is_err());
        service.close().await.unwrap();
    }
}
