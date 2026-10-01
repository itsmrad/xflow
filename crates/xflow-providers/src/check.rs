use super::*;
#[derive(Clone, Debug)]
pub struct CheckReport {
    pub ok: bool,
    pub latency_ms: u64,
    pub detail: String,
}
pub(crate) fn probe_url(
    endpoint: &Url,
    provider: &str,
    sync: Option<bool>,
) -> Result<(reqwest::Method, Url)> {
    let mut url = endpoint.clone();
    if sync == Some(true) && is_canonical_host(endpoint, "sync.assemblyai.com") {
        url = Url::parse("https://api.assemblyai.com/v2/transcript")
            .map_err(|_| anyhow!("invalid check endpoint"))?;
        url.query_pairs_mut().append_pair("limit", "1");
        return Ok((reqwest::Method::GET, url));
    }
    let path = match provider {
        "openai" => "/v1/models",
        "groq" => "/openai/v1/models",
        "openrouter" => "/api/v1/key",
        "deepgram" => "/v1/auth/token",
        "assemblyai" => "/v2/transcript",
        "elevenlabs" => "/v1/user",
        "together" | "mistral" => "/v1/models",
        "deepinfra" => "/v1/me",
        "fireworks" => "/inference/v1/models",
        "gemini" => "/v1beta/models",
        _ => {
            // Only derive a model-list path from documented compatible routes.
            for suffix in ["/audio/transcriptions", "/chat/completions"] {
                if let Some(prefix) = endpoint.path().strip_suffix(suffix) {
                    url.set_path(&format!("{prefix}/models"));
                    return Ok((reqwest::Method::GET, url));
                }
            }
            if endpoint.path().ends_with("/inference") {
                url.set_path("/health");
                return Ok((reqwest::Method::GET, url));
            }
            return Ok((reqwest::Method::HEAD, url));
        }
    };
    url.set_path(path);
    if provider == "assemblyai" {
        url.query_pairs_mut().append_pair("limit", "1");
    }
    if provider == "gemini" {
        url.query_pairs_mut().append_pair("pageSize", "1");
    }
    Ok((reqwest::Method::GET, url))
}
pub(crate) async fn check_request(
    client: &Client,
    credential: &Credentials,
    auth: Auth,
    method: reqwest::Method,
    url: Url,
    timeout: Duration,
) -> Result<CheckReport> {
    let start = std::time::Instant::now();
    let result=tokio::time::timeout(timeout,async {
        let response=credential.apply(client.request(method,url),auth).await?.send().await.map_err(http_error)?;
        let status=response.status();
        if matches!(status.as_u16(),401|403) {credential.invalidate().await;}
        read_bytes(response,false).await?;
        Ok::<_,anyhow::Error>((status.is_success(),match status.as_u16() {
            200..=299=>"Endpoint accepted the non-billing check; inference/model permissions may differ".to_owned(),
            401=>"Authentication rejected; check the key".to_owned(),
            403=>"Access denied; the key may be restricted or invalid".to_owned(),
            code=>format!("Provider returned HTTP {code}; response details omitted"),
        }))
    }).await;
    let (ok, detail) = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => (false, e.to_string()),
        Err(_) => (false, "Provider check timed out".into()),
    };
    Ok(CheckReport {
        ok,
        detail,
        latency_ms: start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}
pub async fn check_cleanup(config: &CleanupConfig, offline: bool) -> Result<CheckReport> {
    if config.mode == CleanupMode::Raw && config.provider.is_none() && config.endpoint.is_none() {
        return Ok(CheckReport {
            ok: true,
            latency_ms: 0,
            detail: "Cleanup is disabled".into(),
        });
    }
    let transformer = HttpTransformer::new(config, offline)?;
    let (method, url) = probe_url(&transformer.endpoint, &transformer.provider, None)?;
    let auth = if transformer.provider == "gemini" {
        Auth::Raw("x-goog-api-key")
    } else {
        Auth::Bearer
    };
    check_request(
        &transformer.client,
        &transformer.credential,
        auth,
        method,
        url,
        transformer.timeout,
    )
    .await
}
