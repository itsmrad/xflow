//! Provider and device discovery stays here; rendering never accesses services.
use anyhow::{bail, Result};
use xflow_core::config::Config;
use xflow_providers::{find_provider, KeySource, ProviderKind};

#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub models: Vec<String>,
    pub key_url: String,
}
pub fn catalog(cleanup: bool) -> Vec<Provider> {
    let catalog = if cleanup {
        xflow_providers::cleanup_providers()
    } else {
        xflow_providers::stt_providers()
    };
    catalog
        .iter()
        .map(|p| Provider {
            id: p.id.into(),
            name: p.name.into(),
            models: p.models.iter().map(|m| m.id.into()).collect(),
            key_url: p.key_url.into(),
        })
        .collect()
}
/// A new preset resets routing overrides belonging to the old preset.
/// Selecting its model also activates that preset; reselecting preserves overrides.
pub fn selection(config: &Config, id: &str, cleanup: bool, model: Option<&str>) -> Result<Config> {
    let kind = if cleanup {
        ProviderKind::Cleanup
    } else {
        ProviderKind::Stt
    };
    find_provider(kind, id).ok_or_else(|| anyhow::anyhow!("Unknown provider"))?;
    if model
        .is_some_and(|m| m.trim().is_empty() || m.len() > 256 || m.chars().any(char::is_control))
    {
        bail!("Model must contain 1..256 bytes without control characters");
    }
    let mut selected = config.clone();
    if cleanup {
        if selected.cleanup.provider.as_deref() != Some(id) {
            selected.cleanup.provider = Some(id.into());
            selected.cleanup.endpoint = None;
            selected.cleanup.model = None;
            selected.cleanup.api_key_env = "OPENAI_API_KEY".into();
        }
        if let Some(model) = model {
            selected.cleanup.model = Some(model.into());
        }
    } else {
        if selected.stt.provider != id {
            selected.stt.provider = id.into();
            selected.stt.endpoint = None;
            selected.stt.model = None;
            selected.stt.protocol = None;
            selected.stt.api_key_env = None;
        }
        if let Some(model) = model {
            selected.stt.model = Some(model.into());
        }
    }
    selected.validate()?;
    Ok(selected)
}
/// Credential state per route; a pending prompt is neither "missing" nor "added".
pub async fn missing_keys(config: &Config) -> (KeySource, KeySource) {
    // Custom servers can be anonymous on loopback; their connection check is
    // authoritative. Built-in local servers need no default credential.
    let stt = if matches!(config.stt.provider.as_str(), "local" | "custom") {
        KeySource::NotRequired
    } else {
        xflow_providers::key_status(&config.stt.provider, config.stt.api_key_env.as_deref()).await
    };
    let cleanup = match config.cleanup.provider.as_deref() {
        Some(id) if config.cleanup.mode != xflow_core::CleanupMode::Raw => {
            let configured = &config.cleanup.api_key_env;
            let env = if configured.is_empty() {
                None
            } else if configured == "OPENAI_API_KEY" {
                find_provider(ProviderKind::Cleanup, id).and_then(|p| p.env_var)
            } else {
                Some(configured.as_str())
            };
            xflow_providers::key_status(xflow_providers::cleanup_key_id(id), env).await
        }
        _ => KeySource::NotRequired,
    };
    (stt, cleanup)
}
/// Re-save Keychain keys once per signature of the xflow binaries, so a key saved
/// by an older (or differently signed) build is readable by xflowd without asking.
/// Returns whether anything was re-saved.
pub async fn share_keys(ids: Vec<String>) -> Result<bool> {
    let exe = std::env::current_exe()?;
    // codesign prints each designated requirement on stdout.
    let signature = tokio::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .args(["xflow", "xflowd"].map(|name| exe.with_file_name(name)))
        .output()
        .await?
        .stdout;
    let dir = crate::paths::data_dir()?;
    let marker = dir.join("keychain-signature");
    if std::fs::read(&marker).is_ok_and(|seen| seen == signature) {
        return Ok(false);
    }
    for id in &ids {
        xflow_providers::share_key(id).await?;
    }
    crate::paths::private_dir(&dir)?;
    std::fs::write(&marker, signature)?;
    Ok(true)
}
/// Non-billable key/reachability probe, answered in plain words.
pub async fn check(config: &Config, cleanup: bool) -> Result<String> {
    let report = if cleanup {
        xflow_providers::check_cleanup(&config.cleanup, config.privacy.offline).await?
    } else {
        xflow_providers::check_stt(&config.stt, config.privacy.offline).await?
    };
    if report.ok {
        Ok(format!(
            "Connected — everything works ({:.1} s).",
            report.latency_ms as f64 / 1000.0
        ))
    } else {
        bail!("{}", report.detail)
    }
}
pub fn devices() -> Result<Vec<String>> {
    // Device enumeration never opens or records from an input stream.
    Ok(xflow_platform::input_devices()?
        .into_iter()
        .map(|d| d.name)
        .collect())
}
