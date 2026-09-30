use anyhow::Result;
use tokio::sync::broadcast;
use xflow_core::DesktopEvent;
use zbus::{interface, ConnectionBuilder, SignalContext};

struct DaemonBridge;

#[interface(name = "org.xflow.Daemon")]
impl DaemonBridge {
    #[zbus(signal)]
    async fn event(context: &SignalContext<'_>, json: &str) -> zbus::Result<()>;
}

/// Owns one session-bus connection until the daemon's event channel closes.
/// Callers may treat an unavailable session bus as a nonfatal UI failure.
pub async fn run_desktop_bridge(mut events: broadcast::Receiver<DesktopEvent>) -> Result<()> {
    let connection = ConnectionBuilder::session()?
        .name("org.xflow.Daemon")?
        .serve_at("/org/xflow/Daemon", DaemonBridge)?
        .build()
        .await?;
    let context = SignalContext::new(&connection, "/org/xflow/Daemon")?;
    loop {
        match events.recv().await {
            Ok(event) => DaemonBridge::event(&context, &serde_json::to_string(&event)?).await?,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    // Dropping the connection leaves its socket reader alive briefly. Release
    // the well-known name on the bus before reporting bridge shutdown.
    connection.release_name("org.xflow.Daemon").await?;
    connection.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use xflow_core::State;
    use zbus::export::futures_util::StreamExt;

    #[tokio::test]
    #[ignore = "requires an isolated session bus: dbus-run-session cargo test -p xflow-platform -- --ignored"]
    async fn bridge_emits_json_and_exits_when_channel_closes() {
        let (send, receive) = broadcast::channel(8);
        let connection = zbus::Connection::session().await.unwrap();
        let bus = zbus::Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .unwrap();
        let mut owners = bus.receive_signal("NameOwnerChanged").await.unwrap();
        let bridge = tokio::spawn(run_desktop_bridge(receive));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let signal = owners.next().await.unwrap();
                let (name, _old, new): (String, String, String) =
                    signal.body().deserialize().unwrap();
                if name == "org.xflow.Daemon" && !new.is_empty() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let proxy = zbus::Proxy::new(
            &connection,
            "org.xflow.Daemon",
            "/org/xflow/Daemon",
            "org.xflow.Daemon",
        )
        .await
        .unwrap();
        let mut signals = proxy.receive_signal("Event").await.unwrap();
        send.send(DesktopEvent {
            state: State::Listening,
            level: 0.25,
            message: None,
        })
        .unwrap();
        let signal = tokio::time::timeout(Duration::from_secs(3), signals.next())
            .await
            .unwrap()
            .unwrap();
        let (json,): (String,) = signal.body().deserialize().unwrap();
        let event: DesktopEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.state, State::Listening);
        assert_eq!(event.level, 0.25);
        drop(send);
        tokio::time::timeout(Duration::from_secs(1), bridge)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let owned: bool = bus
            .call("NameHasOwner", &("org.xflow.Daemon",))
            .await
            .unwrap();
        assert!(!owned, "bridge must release its bus name at shutdown");
    }
}
