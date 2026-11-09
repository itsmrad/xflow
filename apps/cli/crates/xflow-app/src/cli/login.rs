use super::{config, ipc, output::Output, Error, Result};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use zeroize::Zeroizing;

const ORIGIN: &str = "https://xflowcli.vercel.app";
fn fail(message: &str) -> Error {
    Error::new(5, message, "Run xflow login to try again")
}
async fn post(client: &Client, url: &str, body: Value) -> Result<Value> {
    let mut response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|_| fail("Sign-in server unavailable"))?;
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| fail("Sign-in reply interrupted"))?
    {
        if bytes.len() + chunk.len() > 16 * 1024 {
            return Err(fail("Invalid sign-in reply"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| fail("Invalid sign-in reply"))
}
async fn login(origin: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| fail("Cannot initialize sign-in"))?;
    let reply = post(
        &client,
        &format!("{origin}/api/v1/device/code"),
        json!({"client_name":"xflow CLI"}),
    )
    .await?;
    let code = Zeroizing::new(
        reply["device_code"]
            .as_str()
            .ok_or_else(|| fail("Invalid device code"))?
            .to_owned(),
    );
    let user_code = reply["user_code"]
        .as_str()
        .filter(|c| c.len() <= 16 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .ok_or_else(|| fail("Invalid user code"))?;
    let uri = reply["verification_uri"]
        .as_str()
        .and_then(|s| Url::parse(s).ok())
        .filter(|u| u.as_str() == format!("{origin}/device"))
        .ok_or_else(|| fail("Invalid verification URL"))?;
    let expires = reply["expires_in"]
        .as_u64()
        .filter(|s| (1..=900).contains(s))
        .ok_or_else(|| fail("Invalid device expiry"))?;
    let mut interval = reply["interval"].as_u64().unwrap_or(5).clamp(5, 60);
    eprintln!("Open {uri} and enter {user_code}. Approve only if you started this sign-in.");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(expires);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(fail("Sign-in code expired"));
        }
        let mut reply = post(&client, &format!("{origin}/api/v1/device/token"), json!({"device_code":code.as_str(),"grant_type":"urn:ietf:params:oauth:grant-type:device_code"})).await?;
        if let Some(token) = reply
            .get_mut("access_token")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
        {
            let token = Zeroizing::new(token);
            if !token.starts_with("xf_") || token.len() > 256 {
                return Err(fail("Invalid access token"));
            }
            xflow_providers::save_key("hosted", &token)
                .await
                .map_err(|_| fail("Could not save the sign-in in the encrypted keystore"))?;
            return Ok(());
        }
        match reply["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => {
                interval = (interval + 5)
                    .max(reply["interval"].as_u64().unwrap_or(0))
                    .min(60)
            }
            Some("access_denied") => return Err(fail("Sign-in was denied")),
            Some("expired_token") => return Err(fail("Sign-in code expired")),
            _ => return Err(fail("Sign-in failed")),
        }
    }
}
pub fn run(path: &Path, out: &Output) -> Result<()> {
    if config::load(path)?
        .config()
        .map_err(Error::config)?
        .privacy
        .offline
    {
        return Err(fail(
            "Sign-in needs network access; privacy.offline is enabled",
        ));
    }
    ipc::block_on(async {
        tokio::select! {
            result = login(ORIGIN) => result,
            _ = tokio::signal::ctrl_c() => Err(Error::new(130, "Cancelled", "")),
        }
    })?;
    out.data(
        &json!({"ok":true}),
        "Signed in. Use xflow ask --provider hosted; choose hosted under [ai] to keep using it.",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    #[tokio::test]
    async fn device_flow_polls_and_denial_never_saves_a_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let url = origin.clone();
        let server = std::thread::spawn(move || {
            for (path, body) in [
                (
                    "/api/v1/device/code",
                    json!({"device_code":"synthetic-device-code","user_code":"ABCD-EFGH","verification_uri":format!("{url}/device"),"expires_in":60,"interval":5}),
                ),
                ("/api/v1/device/token", json!({"error":"access_denied"})),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut bytes = [0; 8192];
                let n = socket.read(&mut bytes).unwrap();
                assert!(String::from_utf8_lossy(&bytes[..n]).starts_with(&format!("POST {path} ")));
                let body = body.to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let error = login(&origin).await.unwrap_err();
        assert!(error.to_string().contains("denied"));
        server.join().unwrap();
    }
}
