//! Wiring for the provider worker's forthcoming catalog/credential/file APIs.
use super::{args, output::Output, Error, Result};
use std::path::Path;
fn pending() -> Error {
    Error::new(
        5,
        "Provider catalog integration is pending",
        "Use the matching provider milestone from the coordinator",
    )
}
pub fn run(_path: &Path, _action: Option<&args::Providers>, _out: &Output) -> Result<()> {
    Err(pending())
}
pub fn models(_path: &Path, _provider: Option<&str>, _out: &Output) -> Result<()> {
    Err(pending())
}
pub fn key(
    _path: &Path,
    action: Option<&args::Key>,
    provider: Option<&str>,
    stdin: bool,
    out: &Output,
) -> Result<()> {
    match action {
        Some(args::Key::Set { provider, stdin }) => save_key(provider, *stdin, out),
        None if provider.is_some() => save_key(provider.unwrap(), stdin, out),
        _ => Err(pending()),
    }
}
pub fn save_key(provider: &str, stdin: bool, out: &Output) -> Result<()> {
    use std::io::{IsTerminal, Read};
    let secret = if stdin {
        let mut value = String::new();
        std::io::stdin().take(4097).read_to_string(&mut value)?;
        if value.len() > 4096 {
            return Err(Error::new(
                2,
                "API key is too long",
                "Provide at most 4096 bytes",
            ));
        }
        value.trim().to_owned()
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(Error::new(
                2,
                "API key requires input",
                "Use --stdin in scripts",
            ));
        }
        rpassword::prompt_password("API key (hidden): ")?
    };
    if secret.is_empty() {
        return Err(Error::new(
            2,
            "API key cannot be empty",
            "Supply a provider key",
        ));
    }
    super::ipc::block_on(async {
        xflow_providers::save_key(provider, &secret)
            .await
            .map_err(Error::provider)
    })?;
    out.confirm("Key stored in OS credential storage")?;
    Ok(())
}
pub fn transcribe(
    _path: &Path,
    _file: &Path,
    _provider: Option<&str>,
    _model: Option<&str>,
    _language: Option<&str>,
    _out: &Output,
) -> Result<()> {
    Err(pending())
}
pub fn devices(_out: &Output) -> Result<()> {
    Err(Error::new(
        1,
        "Input device catalog integration is pending",
        "Use the matching platform milestone from the coordinator",
    ))
}
