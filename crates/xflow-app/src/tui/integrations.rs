//! The only provider/platform API seam. Replace this compatibility adapter when
//! the coordinator delivers the catalog milestone; never guess keyring status.
use anyhow::{bail, Result};
use xflow_core::config::Config;

#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub models: Vec<String>,
    pub key: String,
    pub note: String,
}
pub async fn providers(config: &Config, cleanup: bool) -> Vec<Provider> {
    // ponytail: use the adapters already in this branch; the shared catalog API
    // replaces this list when the providers worker's milestone is integrated.
    [
        ("groq", "Groq", "whisper-large-v3-turbo", "GROQ_API_KEY"),
        ("openai", "OpenAI", "whisper-1", "OPENAI_API_KEY"),
        (
            "openrouter",
            "OpenRouter",
            "openai/whisper-large-v3",
            "OPENROUTER_API_KEY",
        ),
        ("custom", "Custom endpoint", "", ""),
    ]
    .into_iter()
    .map(|(id, name, model, env)| {
        let env = if cleanup {
            Some(config.cleanup.api_key_env.as_str()).filter(|v| !v.is_empty())
        } else {
            config
                .stt
                .api_key_env
                .as_deref()
                .or(Some(env).filter(|v| !v.is_empty()))
        };
        let key = match env {
            Some(env) if std::env::var_os(env).is_some_and(|v| !v.is_empty()) => {
                format!("Environment: {env}")
            }
            _ => "Environment missing; keyring not checked".into(),
        };
        Provider {
            id: id.into(),
            name: name.into(),
            models: vec![model.into()],
            key,
            note: if cleanup {
                "Set endpoint and model in Settings; cleanup presets await catalog integration"
            } else {
                "Set key: k  •  Model: m  •  Activate: Enter"
            }
            .into(),
        }
    })
    .collect()
}
pub async fn check(config: &Config, cleanup: bool) -> Result<String> {
    if cleanup {
        xflow_providers::build_transformer(&config.cleanup, config.privacy.offline)?;
    } else {
        xflow_providers::build_stt(&config.stt, config.privacy.offline)?;
    }
    Ok("Configuration valid; connection check awaits provider catalog integration".into())
}
pub async fn delete_key(_id: &str) -> Result<()> {
    bail!("Key deletion awaits provider catalog integration")
}
pub fn devices() -> Result<Vec<String>> {
    // Device enumeration never opens or records from an input stream.
    Ok(xflow_platform::input_devices()?
        .into_iter()
        .map(|d| d.name)
        .collect())
}
