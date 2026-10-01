use anyhow::Result;
use tokio::sync::{broadcast, mpsc, oneshot};
use xflow_core::{
    ipc::{Command, Request},
    DesktopEvent,
};
use zbus::{fdo, interface, ConnectionBuilder, SignalContext};

struct DaemonBridge {
    // Weak: the bridge must not keep the daemon's command channel open at shutdown.
    commands: mpsc::WeakSender<Command>,
}

#[interface(name = "org.xflow.Daemon")]
impl DaemonBridge {
    /// Same JSON request/response as the Unix socket, limited to the controls
    /// the desktop shell needs. See docs/CONTRACTS.md.
    async fn command(&self, request: &str) -> fdo::Result<String> {
        if request.len() > xflow_core::ipc::MAX_MESSAGE_BYTES {
            return Err(fdo::Error::InvalidArgs(
                "xflow request exceeds 64 KiB".into(),
            ));
        }
        let request: Request = serde_json::from_str(request)
            .map_err(|_| fdo::Error::InvalidArgs("invalid xflow request".into()))?;
        if !matches!(
            request,
            Request::Status
                | Request::Start { .. }
                | Request::Stop
                | Request::Toggle { .. }
                | Request::Cancel
                | Request::CopyLast
                | Request::PasteLast
        ) {
            return Err(fdo::Error::AccessDenied(
                "request is not available over D-Bus".into(),
            ));
        }
        let stopping = || fdo::Error::Failed("xflow daemon is stopping".into());
        let commands = self.commands.upgrade().ok_or_else(stopping)?;
        let (reply, response) = oneshot::channel();
        commands
            .send(Command { request, reply })
            .await
            .map_err(|_| stopping())?;
        let response = response.await.map_err(|_| stopping())?;
        serde_json::to_string(&response).map_err(|_| fdo::Error::Failed("encoding failed".into()))
    }

    #[zbus(signal)]
    async fn event(context: &SignalContext<'_>, json: &str) -> zbus::Result<()>;
}

/// Owns one session-bus connection until the daemon's event channel closes.
/// Callers may treat an unavailable session bus as a nonfatal UI failure.
pub async fn run_desktop_bridge(
    mut events: broadcast::Receiver<DesktopEvent>,
    commands: mpsc::WeakSender<Command>,
) -> Result<()> {
    let connection = ConnectionBuilder::session()?
        .name("org.xflow.Daemon")?
        .serve_at("/org/xflow/Daemon", DaemonBridge { commands })?
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
    use xflow_core::{ipc::Response, State};
    use zbus::export::futures_util::StreamExt;

    #[tokio::test]
    #[ignore = "requires an isolated session bus: dbus-run-session cargo test -p xflow-platform -- --ignored"]
    async fn bridge_emits_json_and_exits_when_channel_closes() {
        let (send, receive) = broadcast::channel(8);
        let (commands, mut requests) = mpsc::channel::<Command>(1);
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
        let bridge = tokio::spawn(run_desktop_bridge(receive, commands.downgrade()));
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
            mode: Default::default(),
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

        // Commands are forwarded to the daemon actor; history stays socket-only.
        let actor = tokio::spawn(async move {
            let command = requests.recv().await.unwrap();
            assert_eq!(command.request, Request::toggle());
            let _ = command.reply.send(Response::status(State::Listening, 0.0));
        });
        let reply: String = proxy
            .call("Command", &(r#"{"command":"toggle"}"#,))
            .await
            .unwrap();
        assert!(reply.contains(r#""state":"listening""#));
        actor.await.unwrap();
        assert!(proxy
            .call::<_, _, String>("Command", &(r#"{"command":"history","limit":1}"#,))
            .await
            .is_err());

        drop(commands);
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
