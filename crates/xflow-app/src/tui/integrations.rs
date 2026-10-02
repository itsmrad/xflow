//! Provider and device discovery stays here; rendering never accesses services.
use anyhow::{bail, Result};
use xflow_core::config::Config;
use xflow_providers::{find_provider, KeySource, ProviderKind};

#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub models: Vec<String>,
    pub key: String,
    pub note: String,
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
            key: "Checking credentials…".into(),
            note: format!(
                "Endpoint: {}\n{}\n{}\n{}",
                p.endpoint,
                p.models
                    .iter()
                    .map(|m| format!("{} — {}", m.label, m.note))
                    .collect::<Vec<_>>()
                    .join("\n"),
                p.key_url,
                p.docs_url
            ),
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
pub async fn providers(config: &Config, cleanup: bool) -> Vec<Provider> {
    // Catalog appears immediately; bounded credential lookups stay off the UI task.
    futures_util::future::join_all(catalog(cleanup).into_iter().map(|mut p| async move {
        let selected = selection(config, &p.id, cleanup, None).expect("static catalog provider");
        let env = if cleanup {
            let value = &selected.cleanup.api_key_env;
            if value.is_empty() {
                None
            } else if value == "OPENAI_API_KEY" {
                find_provider(ProviderKind::Cleanup, &p.id).and_then(|p| p.env_var)
            } else {
                Some(value.as_str())
            }
        } else {
            selected.stt.api_key_env.as_deref()
        };
        p.key = match xflow_providers::key_status(&p.id, env).await {
            KeySource::Environment(name) => format!("Environment: {name}"),
            KeySource::Keyring => "Keyring: saved".into(),
            KeySource::NotRequired => "Key not required".into(),
            KeySource::Missing => "Missing key — k adds one".into(),
        };
        p
    }))
    .await
}
pub async fn check(config: &Config, cleanup: bool) -> Result<String> {
    let report = if cleanup {
        xflow_providers::check_cleanup(&config.cleanup, config.privacy.offline).await?
    } else {
        xflow_providers::check_stt(&config.stt, config.privacy.offline).await?
    };
    Ok(format!(
        "{} · {} ms · {}",
        if report.ok { "✓" } else { "!" },
        report.latency_ms,
        report.detail
    ))
}
pub async fn missing_key(config: &Config) -> bool {
    // Custom servers can be anonymous on loopback; their connection check is
    // authoritative. Built-in local servers need no default credential.
    if matches!(config.stt.provider.as_str(), "local" | "custom") {
        return false;
    }
    matches!(
        xflow_providers::key_status(&config.stt.provider, config.stt.api_key_env.as_deref()).await,
        KeySource::Missing
    )
}
pub async fn delete_key(id: &str) -> Result<()> {
    xflow_providers::delete_key(id).await
}
pub fn devices() -> Result<Vec<String>> {
    // Device enumeration never opens or records from an input stream.
    Ok(xflow_platform::input_devices()?
        .into_iter()
        .map(|d| d.name)
        .collect())
}
