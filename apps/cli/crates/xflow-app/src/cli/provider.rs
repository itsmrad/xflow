//! All provider/platform API wiring stays here; no independent catalog or credential store.
use super::{args, config, ipc, output::Output, Error, Result};
use std::{
    io::{IsTerminal, Read, Write},
    path::Path,
};
use xflow_app::config_edit;
use xflow_core::config::{Config, SttConfig};
use xflow_providers::{KeySource, ProviderInfo, ProviderKind, ON_DEVICE};

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
            .or_else(|| xflow_providers::find_provider(ProviderKind::Chat, id))
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
        "env_var":p.env_var,"key_url":p.key_url,"docs_url":p.docs_url,"language":p.language,"vocabulary":p.vocabulary,"local":p.local,"language_sets":xflow_providers::language_capability(&SttConfig {provider:p.id.into(),..SttConfig::default()}).unwrap_or("depends on server/model")})
}
fn on_device_stt() -> SttConfig {
    SttConfig {
        provider: ON_DEVICE.into(),
        ..SttConfig::default()
    }
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
    let file = config::load(path)?;
    let mut cfg = file.config().map_err(Error::config)?;
    match action.unwrap_or(&List) {
        List => {
            let providers = xflow_providers::stt_providers();
            let on_device = cfg.stt.provider == ON_DEVICE;
            if out.json {
                let mut rows: Vec<_> = providers
                    .iter()
                    .map(|p| {
                        let mut v = value(p);
                        v["current"] = (p.id == cfg.stt.provider).into();
                        v
                    })
                    .collect();
                let model = xflow_providers::on_device::MODEL_ID;
                rows.push(serde_json::json!({"id":ON_DEVICE,"name":"On-device (Apple Silicon)","endpoint":"","default_model":model,
                    "models":[{"id":model,"label":"Parakeet TDT 0.6B v3","note":"Install with xflow models install; runs on this Mac"}],
                    "env_var":null,"key_url":"","docs_url":"","language":true,"vocabulary":false,"local":true,
                    "language_sets":"auto-detect (25 European languages)","current":on_device}));
                out.data(&rows, "")?;
            } else {
                let mut rows: Vec<Vec<String>> = providers
                    .iter()
                    .map(|p| {
                        vec![
                            p.id.into(),
                            p.name.into(),
                            p.default_model.into(),
                            if p.id == cfg.stt.provider { "*" } else { "" }.into(),
                        ]
                    })
                    .collect();
                rows.push(vec![
                    ON_DEVICE.into(),
                    "On-device (Apple Silicon)".into(),
                    xflow_providers::on_device::MODEL_ID.into(),
                    if on_device { "*" } else { "" }.into(),
                ]);
                out.table(&["PROVIDER", "NAME", "DEFAULT MODEL", "CURRENT"], &rows)?;
            }
        }
        Info { provider } if provider == ON_DEVICE => {
            let facts = xflow_app::local_models::facts();
            let model = xflow_providers::on_device::MODEL_ID;
            let next = if !facts.supported {
                "Keep an API provider: xflow providers list".to_string()
            } else if !facts.installed {
                format!("Install: xflow models install {model}")
            } else {
                "Use: xflow providers use on-device".to_string()
            };
            out.data(
                &serde_json::json!({"id":ON_DEVICE,"name":facts.name,"model":model,
                    "languages":facts.languages,"unsupported_languages":facts.warning,"download_bytes":facts.download_bytes,
                    "license":facts.license,"fit":facts.fit,"supported":facts.supported,"installed":facts.installed,
                    "current":cfg.stt.provider == ON_DEVICE}),
                &format!(
                    "On this Mac (offline): speech never leaves this computer; no key needed.\n{}\n{next}",
                    facts.lines().join("\n")
                ),
            )?;
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
                            "Preferred languages".into(),
                            xflow_providers::language_capability(&SttConfig {
                                provider: p.id.into(),
                                ..SttConfig::default()
                            })
                            .unwrap_or("depends on server/model")
                            .into(),
                        ],
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
            if provider.as_deref() == Some(ON_DEVICE) {
                cfg.stt = on_device_stt();
            } else if let Some(id) = provider {
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
            let lock = config::lock(path)?;
            // Read again under the lock: an edit made meanwhile (a learned word) is kept.
            let mut file = config::load(path)?;
            let current = file.config().map_err(Error::config)?.stt;
            if provider == ON_DEVICE {
                if model.is_some() {
                    return Err(Error::new(
                        2,
                        "On-device speech has one model",
                        "Omit --model",
                    ));
                }
                super::local_models::ready_to_use()?;
                config_edit::use_on_device(&mut file).map_err(Error::config)?;
            } else {
                // Back to the API route saved when on-device was chosen, then the new choice.
                let mut stt = if current.provider == ON_DEVICE {
                    config_edit::leave_on_device(&mut file).map_err(Error::config)?
                } else {
                    current
                };
                select(&mut stt, provider, model.as_deref())?;
                file.set_typed("stt", &stt).map_err(Error::config)?;
            }
            config::commit(&file, lock, out)?;
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
            let note = ipc::block_on(async {
                xflow_providers::delete_key(provider)
                    .await
                    .map_err(Error::provider)
            })?;
            ipc::reload_if_running(path, out)?;
            out.confirm(&match note {
                Some(note) => format!("Stored key removed; {note}"),
                None => "Stored key removed; environment overrides are unchanged".into(),
            })?;
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
    move_legacy_keys();
    let source = ipc::block_on(async { Ok(xflow_providers::key_status(id, override_env).await) })?;
    let environment = match &source {
        KeySource::Environment(name) => Some(name.clone()),
        _ => None,
    };
    let (source, note) = describe(id, &source);
    out.data(
        &serde_json::json!({"provider":id,"source":source,"environment":environment}),
        &format!("{id}: {note}"),
    )?;
    Ok(())
}
/// Move keys an older xflow saved in the OS keychain. Only a person at a terminal
/// is asked, after one line saying why; scripts and xflowd never are.
pub fn move_legacy_keys() {
    let notice = || eprintln!("{}", xflow_providers::MOVING_KEYS);
    let foreground = std::io::stderr().is_terminal() && std::io::stdin().is_terminal();
    let _ = xflow_providers::migrate_keychain(foreground.then_some(&notice as &dyn Fn()));
}
/// (JSON source, human description) for `key status` and `doctor`.
pub fn describe(id: &str, source: &KeySource) -> (&'static str, String) {
    match source {
        KeySource::Environment(name) => ("environment", format!("environment ({name})")),
        KeySource::Keystore => ("keystore", "xflow keystore (encrypted file)".into()),
        KeySource::NotRequired => ("not_required", "not required".into()),
        KeySource::Missing => ("missing", format!("missing — run xflow key set {id}")),
        KeySource::Keychain if cfg!(target_os = "macos") => ("keychain", format!(
            "keychain (not migrated: macOS must confirm the move once) — run xflow key status in a terminal and click Allow, or xflow key set {id}"
        )),
        KeySource::Keychain => ("keychain", format!(
            "system keyring (not migrated) — run xflow key status in a terminal, or xflow key set {id}"
        )),
        KeySource::Unreadable(why) => ("unreadable", format!("unreadable: {why}")),
    }
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
        let mut tries = 0;
        loop {
            let key = zeroize::Zeroizing::new(rpassword::prompt_password("API key (hidden): ")?);
            tries += 1;
            if !key.trim().is_empty() || tries == 3 {
                break key;
            }
            eprintln!("Nothing was entered. Paste your key, then press Enter.");
        }
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
    out.confirm("Key stored, encrypted, in the xflow keystore")?;
    Ok(())
}
pub fn ask(path: &Path, question: &[String], out: &Output) -> Result<()> {
    let cfg = config::load(path)?.config().map_err(Error::config)?;
    cfg.validate().map_err(Error::config)?;
    let question = if question.is_empty() {
        let mut stdin = String::new();
        std::io::stdin().read_to_string(&mut stdin)?;
        stdin
    } else {
        question.join(" ")
    };
    let question = question.trim().to_owned();
    if question.is_empty() {
        return Err(Error::new(
            2,
            "Ask needs a question",
            "Pass it as arguments, or pipe it on stdin",
        ));
    }
    if out.json {
        return Err(Error::new(
            2,
            "xflow ask streams text",
            "Run it without --json",
        ));
    }
    ipc::block_on(async {
        let chat = xflow_providers::build_chat(&cfg).map_err(Error::provider)?;
        let target = chat.target();
        let stt = cfg.stt.provider.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn(async move { chat.stream(&question, tx).await });
        while let Some(delta) = rx.recv().await {
            print!("{delta}");
            std::io::stdout().flush().ok();
        }
        task.await
            .map_err(Error::provider)?
            .map_err(Error::provider)?;
        println!("\n{}", xflow_core::privacy_line(&stt, &target));
        Ok(())
    })
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
    if provider == Some(ON_DEVICE) {
        cfg.stt = on_device_stt();
    } else if let Some(provider) = provider {
        select(&mut cfg.stt, provider, model)?;
    } else if let Some(model) = model {
        cfg.stt.model = Some(model.into());
    }
    if let Some(language) = language {
        cfg.stt.languages.clear();
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
        #[cfg(target_os = "linux")]
        if devices.is_empty() {
            out.text("No microphone found. Live dictation needs an audio input; xflow transcribe <file> works without one")?;
        }
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
    #[test]
    fn leaving_on_device_for_a_different_provider_restores_then_applies_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[stt]\nprovider = \"groq\"\nmodel = \"whisper-large-v3\"\napi_key_env = \"MY_GROQ\"\nlanguages = [\"en\"]\n").unwrap();
        let mut file = config_edit::ConfigFile::load(&path).unwrap();
        config_edit::use_on_device(&mut file).unwrap();
        let mut stt = config_edit::leave_on_device(&mut file).unwrap();
        select(&mut stt, "openai", None).unwrap();
        file.set_typed("stt", &stt).unwrap();
        let cfg = file.config().unwrap();
        assert_eq!(cfg.stt.provider, "openai");
        assert!(cfg.stt.model.is_none() && cfg.stt.api_key_env.is_none());
        assert_eq!(cfg.stt.languages, ["en"]);
        assert!(cfg.on_device.previous_api.is_none());
    }
}
