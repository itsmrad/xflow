use super::{args, config, output::Output, Error, Result};
use std::{io::IsTerminal, path::Path};
pub fn run(path: &Path, options: &args::Setup, out: &Output) -> Result<()> {
    let interactive = !options.yes && !out.json && std::io::stdin().is_terminal();
    if !options.yes && !interactive {
        return Err(Error::new(
            2,
            "Setup needs an interactive terminal or --yes",
            "Use xflow setup --yes --provider groq --model whisper-large-v3-turbo",
        ));
    }
    let mut file = config::load(path)?;
    let mut current = file.config().map_err(Error::config)?;
    if let Some(provider) = &options.provider {
        current.stt.provider = provider.clone();
    }
    if let Some(model) = &options.model {
        current.stt.model = Some(model.clone());
    }
    if let Some(language) = &options.language {
        current.stt.language = Some(language.clone());
    }
    if let Some(device) = &options.device {
        current.recording.device = Some(device.clone());
    }
    if interactive {
        eprintln!(
            "XFlow setup — keys stay in OS credential storage; cloud providers receive audio."
        );
        current.stt.provider = config::ask(
            &format!("Provider [{}]: ", current.stt.provider),
            &current.stt.provider,
        )?;
        let model = config::ask(
            &format!(
                "Model [{}; blank uses provider default]: ",
                current.stt.model.as_deref().unwrap_or("default")
            ),
            current.stt.model.as_deref().unwrap_or(""),
        )?;
        current.stt.model = if model.is_empty() { None } else { Some(model) };
        let device = config::ask(
            &format!(
                "Microphone name [{}; blank uses default]: ",
                current.recording.device.as_deref().unwrap_or("default")
            ),
            current.recording.device.as_deref().unwrap_or(""),
        )?;
        current.recording.device = if device.is_empty() {
            None
        } else {
            Some(device)
        };
    }
    file.set_typed("stt", &current.stt).map_err(Error::config)?;
    file.set_typed("recording", &current.recording)
        .map_err(Error::config)?;
    config::commit(&file, out)?;
    if options.key_stdin
        || (interactive
            && config::ask("Store a provider key now? [y/N] ", "")?.eq_ignore_ascii_case("y"))
    {
        super::provider::save_key(&current.stt.provider, options.key_stdin, out)?;
    }
    if options.test
        || (interactive
            && config::ask("Test provider connection (no audio upload)? [y/N] ", "")?
                .eq_ignore_ascii_case("y"))
    {
        super::provider::run(path, Some(&args::Providers::Test { provider: None }), out)?;
    }
    if options.install_extension
        || (interactive
            && config::ask("Install GNOME extension? [y/N] ", "")?.eq_ignore_ascii_case("y"))
    {
        super::system::extension(&args::Extension::Install { enable: false }, out)?;
    }
    if options.enable_service
        || (interactive
            && config::ask("Install and enable user service now? [y/N] ", "")?
                .eq_ignore_ascii_case("y"))
    {
        super::system::service(&args::Service::Install { enable: true }, out)?;
    }
    out.confirm("Ready: start xflowd or run xflow daemon start, then xflow toggle. Repeat toggle to finish; xflow listen --once --seconds 5 prints text for scripts.")?;
    Ok(())
}
