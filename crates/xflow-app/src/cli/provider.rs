//! All provider/platform API wiring stays here; no independent catalog or keyring implementation.
use super::{args, config, ipc, output::Output, Error, Result};
use std::{
    io::{IsTerminal, Read},
    path::Path,
};
use xflow_core::config::{Config, SttConfig};
use xflow_providers::{KeySource, ProviderInfo, ProviderKind};

pub fn info(id: &str) -> Result<&'static ProviderInfo> {
    xflow_providers::find_provider(ProviderKind::Stt, id).ok_or_else(|| {
        Error::new(
            5,
            format!("Unknown speech provider: {id}"),
            "Run xflow providers list",
        )
    })
}
pub fn catalog() -> &'static [ProviderInfo] {
    xflow_providers::stt_providers()
}
fn credential_provider(id: &str) -> Result<()> {
    if id == "cleanup"
        || xflow_providers::find_provider(ProviderKind::Stt, id)
            .or_else(|| xflow_providers::find_provider(ProviderKind::Cleanup, id))
            .is_some()
    {
        Ok(())
    } else {
        Err(Error::provider("Unknown credential provider"))
    }
}
fn value(p: &ProviderInfo) -> serde_json::Value {
    serde_json::json!({"id":p.id,"name":p.name,"endpoint":p.endpoint,"default_model":p.default_model,
        "models":p.models.iter().map(|m| serde_json::json!({"id":m.id,"label":m.label,"note":m.note})).collect::<Vec<_>>(),
        "env_var":p.env_var,"key_url":p.key_url,"docs_url":p.docs_url,"language":p.language,"vocabulary":p.vocabulary,"local":p.local})
}
/// Switching presets must not retain another provider's endpoint, model or credential override.
pub fn select(stt: &mut SttConfig, id: &str, model: Option<&str>) -> Result<()> {
    info(id)?;
    if stt.provider != id {
        stt.endpoint = None;
        stt.protocol = None;
        stt.api_key_env = None;
        stt.model = None;
    }
    stt.provider = id.to_owned();
    if let Some(model) = model {
        stt.model = Some(model.into());
    }
    Ok(())
}
pub fn run(path: &Path, action: Option<&args::Providers>, out: &Output) -> Result<()> {
    use args::Providers::*;
    let mut file = config::load(path)?;
    let mut cfg = file.config().map_err(Error::config)?;
    match action.unwrap_or(&List) {
        List => {
            let providers = xflow_providers::stt_providers();
            if out.json {
                out.data(
                    &providers
                        .iter()
                        .map(|p| {
                            let mut v = value(p);
                            v["current"] = (p.id == cfg.stt.provider).into();
                            v
                        })
                        .collect::<Vec<_>>(),
                    "",
                )?;
            } else {
                out.table(
                    &["PROVIDER", "NAME", "DEFAULT MODEL", "CURRENT"],
                    &providers
                        .iter()
                        .map(|p| {
                            vec![
                                p.id.into(),
                                p.name.into(),
                                p.default_model.into(),
                                if p.id == cfg.stt.provider { "*" } else { "" }.into(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                )?;
            }
        }
        Info { provider } => {
            let p = info(provider)?;
            if out.json {
                out.data(&value(p), "")?;
            } else {
                out.table(
                    &["FIELD", "VALUE"],
                    &[
                        vec!["Provider".into(), p.name.into()],
                        vec!["Default model".into(), p.default_model.into()],
                        vec!["Endpoint".into(), p.endpoint.into()],
                        vec![
                            "Key environment".into(),
                            p.env_var.unwrap_or("not required").into(),
                        ],
                        vec!["Key setup".into(), p.key_url.into()],
                        vec!["Documentation".into(), p.docs_url.into()],
                        vec![
                            "Language / vocabulary".into(),
                            format!("{} / {}", p.language, p.vocabulary),
                        ],
                    ],
                )?;
            }
        }
        Test { provider } => {
            if let Some(id) = provider {
                select(&mut cfg.stt, id, None)?;
            }
            let report = check(&cfg)?;
            out.data(&serde_json::json!({"ok":report.ok,"latency_ms":report.latency_ms,"detail":report.detail}), &format!("{} ({} ms)", report.detail, report.latency_ms))?;
            if !report.ok {
                return Err(Error::new(
                    5,
                    "Provider check failed",
                    "Check xflow providers info and xflow key status",
                ));
            }
        }
        Use { provider, model } => {
            select(&mut cfg.stt, provider, model.as_deref())?;
            file.set_typed("stt", &cfg.stt).map_err(Error::config)?;
            config::commit(&file, out)?;
        }
    }
    Ok(())
}
pub fn check(cfg: &Config) -> Result<xflow_providers::CheckReport> {
    ipc::block_on(async {
        xflow_providers::check_stt(&cfg.stt, cfg.privacy.offline)
            .await
            .map_err(Error::provider)
    })
}
pub fn models(path: &Path, id: Option<&str>, out: &Output) -> Result<()> {
    let cfg = config::load(path)?.config().map_err(Error::config)?;
    let p = info(id.unwrap_or(&cfg.stt.provider))?;
    let selected = if cfg.stt.provider == p.id {
        cfg.stt.model.as_deref().unwrap_or(p.default_model)
    } else {
        ""
    };
    if out.json {
        out.data(&serde_json::json!({"provider":p.id,"models":p.models.iter().map(|m| serde_json::json!({"id":m.id,"label":m.label,"note":m.note,"default":m.id == p.default_model,"current":m.id == selected})).collect::<Vec<_>>()}), "")?;
    } else {
        out.table(
            &["MODEL", "LABEL", "DEFAULT", "CURRENT", "NOTE"],
            &p.models
                .iter()
                .map(|m| {
                    vec![
                        m.id.into(),
                        m.label.into(),
                        if m.id == p.default_model { "*" } else { "" }.into(),
                        if m.id == selected { "*" } else { "" }.into(),
                        m.note.into(),
                    ]
                })
                .collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}
pub fn key(
    path: &Path,
    action: Option<&args::Key>,
    legacy: Option<&str>,
    stdin: bool,
    out: &Output,
) -> Result<()> {
    match action {
        Some(args::Key::Set { provider, stdin }) => save_key(path, provider, *stdin, out),
        None if legacy.is_some() => save_key(path, legacy.unwrap(), stdin, out),
        Some(args::Key::Remove { provider }) => {
            credential_provider(provider)?;
            ipc::block_on(async {
                xflow_providers::delete_key(provider)
                    .await
                    .map_err(Error::provider)
            })?;
            ipc::reload_if_running(path, out)?;
            out.confirm("Stored key removed; environment overrides are unchanged")?;
            Ok(())
        }
        Some(args::Key::Status { provider }) => key_status(path, provider.as_deref(), out),
        None => key_status(path, None, out),
    }
}
fn key_status(path: &Path, id: Option<&str>, out: &Output) -> Result<()> {
    let cfg = config::load(path)?.config().map_err(Error::config)?;
    let id = id.unwrap_or(&cfg.stt.provider);
    credential_provider(id)?;
    let override_env = if id == cfg.stt.provider {
        cfg.stt.api_key_env.as_deref()
    } else {
        None
    };
    let source = ipc::block_on(async { Ok(xflow_providers::key_status(id, override_env).await) })?;
    let (source, environment) = match source {
        KeySource::Environment(name) => ("environment", Some(name)),
        KeySource::Keyring => ("keyring", None),
        KeySource::NotRequired => ("not_required", None),
        KeySource::Missing => ("missing", None),
    };
    out.data(
        &serde_json::json!({"provider":id,"source":source,"environment":environment}),
        &format!(
            "{id}: {source}{}",
            environment.map(|v| format!(" ({v})")).unwrap_or_default()
        ),
    )?;
    Ok(())
}
pub fn save_key(path: &Path, provider: &str, stdin: bool, out: &Output) -> Result<()> {
    credential_provider(provider)?;
    let secret = if stdin {
        let mut value = zeroize::Zeroizing::new(String::new());
        std::io::stdin().take(4097).read_to_string(&mut value)?;
        if value.len() > 4096 {
            return Err(Error::new(
                2,
                "API key is too long",
                "Provide at most 4096 bytes",
            ));
        }
        zeroize::Zeroizing::new(value.trim().to_owned())
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(Error::new(
                2,
                "API key requires input",
                "Use --stdin in scripts",
            ));
        }
        zeroize::Zeroizing::new(rpassword::prompt_password("API key (hidden): ")?)
    };
    if secret.len() > 4096 {
        return Err(Error::new(
            2,
            "API key is too long",
            "Provide at most 4096 bytes",
        ));
    }
    if secret.is_empty() {
        return Err(Error::new(
            2,
            "API key cannot be empty",
            "Supply a provider key",
        ));
    }
    ipc::block_on(async {
        xflow_providers::save_key(provider, &secret)
            .await
            .map_err(Error::provider)
    })?;
    ipc::reload_if_running(path, out)?;
    out.confirm("Key stored in OS credential storage")?;
    Ok(())
}
pub fn transcribe(
    path: &Path,
    file: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    language: Option<&str>,
    out: &Output,
) -> Result<()> {
    let mut cfg = config::load(path)?.config().map_err(Error::config)?;
    if let Some(provider) = provider {
        select(&mut cfg.stt, provider, model)?;
    } else if let Some(model) = model {
        cfg.stt.model = Some(model.into());
    }
    if let Some(language) = language {
        cfg.stt.language = Some(language.into());
    }
    cfg.validate().map_err(Error::config)?;
    let transcript = ipc::block_on(async {
        xflow_providers::transcribe_file(&cfg.stt, cfg.privacy.offline, file)
            .await
            .map_err(Error::provider)
    })?;
    out.data(&transcript, &transcript.text)?;
    Ok(())
}
pub fn devices(out: &Output) -> Result<()> {
    let devices = xflow_platform::input_devices()?;
    if out.json {
        out.data(
            &devices
                .iter()
                .map(|d| serde_json::json!({"name":d.name,"default":d.default}))
                .collect::<Vec<_>>(),
            "",
        )?;
    } else {
        out.table(
            &["DEVICE", "DEFAULT"],
            &devices
                .iter()
                .map(|d| vec![d.name.clone(), if d.default { "*" } else { "" }.into()])
                .collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn switching_provider_clears_credential_and_endpoint_overrides() {
        let mut cfg = SttConfig {
            endpoint: Some("https://old.invalid/transcribe".into()),
            api_key_env: Some("OLD_KEY".into()),
            model: Some("old-model".into()),
            ..Default::default()
        };
        select(&mut cfg, "openai", None).unwrap();
        assert!(cfg.endpoint.is_none() && cfg.api_key_env.is_none() && cfg.model.is_none());
        assert!(select(&mut cfg, "nonexistent", None).is_err());
    }
}
