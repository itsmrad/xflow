use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use xflow_core::ipc::{Request, Response, MAX_MESSAGE_BYTES};

pub async fn read_frame<T: DeserializeOwned, R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<T>> {
    let mut line = String::new();
    let read = (&mut *reader)
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_line(&mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if read > MAX_MESSAGE_BYTES || !line.ends_with('\n') {
        bail!("IPC frame too large or unterminated");
    }
    Ok(Some(
        serde_json::from_str(&line).context("invalid IPC message")?,
    ))
}

pub async fn write_frame<T: Serialize, W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &T,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > MAX_MESSAGE_BYTES {
        bail!("IPC response exceeds frame limit");
    }
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(2), writer.write_all(&bytes))
        .await
        .context("IPC write timed out")??;
    Ok(())
}

pub async fn connect() -> Result<UnixStream> {
    let path = crate::paths::runtime_dir()?.join("daemon.sock");
    let stream = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(&path))
        .await
        .context("daemon connection timed out")?
        .with_context(|| format!("cannot connect to {}; start xflowd", path.display()))?;
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        bail!("daemon belongs to another user");
    }
    Ok(stream)
}
pub async fn request(request: &Request) -> Result<Response> {
    let mut stream = connect().await?;
    write_frame(&mut stream, request).await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        read_frame(&mut BufReader::new(stream)),
    )
    .await
    .context("daemon request timed out")??
    .context("daemon closed connection")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_oversized_and_truncated_frames() {
        let bytes = vec![b'a'; MAX_MESSAGE_BYTES + 2];
        assert!(read_frame::<Request, _>(&mut bytes.as_slice())
            .await
            .is_err());
        assert!(
            read_frame::<Request, _>(&mut b"{\"command\":\"status\"}".as_slice())
                .await
                .is_err()
        );
        assert!(matches!(
            read_frame::<Request, _>(&mut b"{\"command\":\"status\"}\n".as_slice())
                .await
                .unwrap(),
            Some(Request::Status)
        ));
    }
}
