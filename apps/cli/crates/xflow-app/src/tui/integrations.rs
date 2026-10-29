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
/// Setup's default (#67): light cleanup on the speech key, when that provider
/// also cleans up and cleanup was never set up. Existing choices are kept.
pub fn smart_formatting(config: &Config) -> Option<Config> {
    let cleanup = &config.cleanup;
    let id = config.stt.provider.as_str();
    if cleanup.mode != xflow_core::CleanupMode::Raw
        || cleanup.provider.is_some()
        || cleanup.endpoint.is_some()
        || matches!(id, "custom" | "local")
    {
        return None;
    }
    let mut smart = selection(config, id, true, None).ok()?;
    smart.cleanup.mode = xflow_core::CleanupMode::Light;
    smart.validate().ok()?;
    Some(smart)
}
/// What setup tells the user it turned on: the key it uses and what it costs.
pub fn smart_line(config: &Config) -> Option<String> {
    let id = config.cleanup.provider.as_deref()?;
    if config.cleanup.mode != xflow_core::CleanupMode::Light || id != config.stt.provider {
        return None;
    }
    let name = find_provider(ProviderKind::Cleanup, id)?.name;
    // Measured on the default model only (benchmarks/formatting).
    let measured = id == "groq"
        && config
            .cleanup
            .model
            .as_deref()
            .is_none_or(|m| m == "qwen/qwen3.8-27b");
    let cost = if measured { " · ~+0.2 s" } else { "" };
    Some(format!("Smart formatting: on · uses your {name} key{cost}"))
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
