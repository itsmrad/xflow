use super::{args, config, output::Output, provider, Error, Result};
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
    let mut id = options
        .provider
        .clone()
        .unwrap_or_else(|| current.stt.provider.clone());
    let mut model = options.model.clone();
    let mut device = options.device.clone();
    if interactive {
        eprintln!(
            "XFlow setup — keys stay in OS credential storage; cloud providers receive audio."
        );
        let providers = provider::catalog();
        for (i, p) in providers.iter().enumerate() {
            eprintln!("  {}. {} ({})", i + 1, p.name, p.id);
        }
        let answer = config::ask(&format!("Provider number or ID [{id}]: "), &id)?;
        id = number_or_name(&answer, providers.iter().map(|p| p.id))?;
        let p = provider::info(&id)?;
        for (i, m) in p.models.iter().enumerate() {
            eprintln!("  {}. {} — {}", i + 1, m.id, m.label);
        }
        let default = model
            .as_deref()
            .or_else(|| {
                if current.stt.provider == id {
                    current.stt.model.as_deref()
                } else {
                    None
                }
            })
            .unwrap_or(p.default_model);
        let answer = config::ask(&format!("Model number or ID [{default}]: "), default)?;
        model = if answer.is_empty() {
            None
        } else {
            Some(number_or_name(&answer, p.models.iter().map(|m| m.id))?)
        };
        match xflow_platform::input_devices() {
            Ok(devices) if !devices.is_empty() => {
                for (i, d) in devices.iter().enumerate() {
                    eprintln!(
                        "  {}. {}{}",
                        i + 1,
                        super::output::clean(&d.name),
                        if d.default { " (default)" } else { "" }
                    );
                }
                let default = device
                    .as_deref()
                    .or(current.recording.device.as_deref())
                    .unwrap_or("");
                let answer = config::ask(
                    &format!("Microphone number or name [{default}; blank uses default]: "),
                    default,
                )?;
                device = if answer.is_empty() {
                    None
                } else {
                    Some(number_or_name(
                        &answer,
                        devices.iter().map(|d| d.name.as_str()),
                    )?)
                };
            }
            Ok(_) | Err(_) => {
                eprintln!("No input devices available; choose recording.device later with xflow devices and xflow config set.");
            }
        }
    }
    provider::select(&mut current.stt, &id, model.as_deref())?;
    if let Some(endpoint) = &options.endpoint {
        current.stt.endpoint = Some(endpoint.clone());
    }
    if interactive && id == "custom" && current.stt.endpoint.is_none() {
        let endpoint = config::ask("Custom transcription endpoint (full URL): ", "")?;
        current.stt.endpoint = Some(endpoint);
    }
    if let Some(language) = &options.language {
        current.stt.language = Some(language.clone());
    }
    if device.is_some() || interactive {
        current.recording.device = device;
    }
    current.validate().map_err(Error::config)?;
    file.set_typed("stt", &current.stt).map_err(Error::config)?;
    file.set_typed("recording", &current.recording)
        .map_err(Error::config)?;
    let quiet = Output {
        json: false,
        quiet: true,
        color: false,
    };
    config::commit(&file, &quiet)?;
    let store_key = options.key_stdin
        || (interactive
            && !provider::info(&id)?.local
            && config::ask("Store a provider key now? [y/N] ", "")?.eq_ignore_ascii_case("y"));
    if store_key {
        provider::save_key(path, &id, options.key_stdin, &quiet)?;
    }
    let test = options.test
        || (interactive
            && config::ask("Test provider connection (no audio upload)? [y/N] ", "")?
                .eq_ignore_ascii_case("y"));
    let report = if test {
        let report = provider::check(&current)?;
        if !report.ok {
            return Err(Error::new(
                5,
                report.detail,
                "Review xflow providers info and xflow key status; configuration was saved",
            ));
        }
        if !out.json && !out.quiet {
            eprintln!(
                "Connection: {} ({} ms)",
                super::output::clean(&report.detail),
                report.latency_ms
            );
        }
        Some(
            serde_json::json!({"ok":report.ok,"latency_ms":report.latency_ms,"detail":report.detail}),
        )
    } else {
        None
    };
    let extension = options.install_extension
        || (interactive
            && config::ask("Install GNOME extension? [y/N] ", "")?.eq_ignore_ascii_case("y"));
    if extension {
        super::system::extension(&args::Extension::Install { enable: false }, &quiet)?;
    }
    let service = options.enable_service
        || (interactive
            && config::ask("Install and enable user service now? [y/N] ", "")?
                .eq_ignore_ascii_case("y"));
    if service {
        super::system::service(&args::Service::Install { enable: true }, &quiet)?;
    }
    let tip = "Start xflowd or run xflow daemon start, then xflow toggle. Repeat toggle to finish; xflow listen --once --seconds 5 prints text for scripts.";
    if out.json {
        out.data(&serde_json::json!({"ok":true,"config":path,"provider":id,"key_stored":store_key,"connection":report,"extension_installed":extension,"service_enabled":service,"tip":tip,"extension_tip":if extension { Some("On Wayland, log out and back in before xflow extension enable") } else { None }}), "")?;
    } else {
        out.confirm(&format!("Ready: {tip}"))?;
        if extension {
            out.confirm("On Wayland, log out and back in, then run xflow extension enable.")?;
        }
    }
    Ok(())
}
fn number_or_name<'a>(answer: &str, values: impl Iterator<Item = &'a str>) -> Result<String> {
    if let Ok(number) = answer.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|index| values.into_iter().nth(index))
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::new(
                    2,
                    "Selection number is out of range",
                    "Choose a displayed number or enter a name",
                )
            });
    }
    Ok(answer.into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_handles_names_and_rejects_invalid_numbers() {
        assert_eq!(number_or_name("2", ["a", "b"].into_iter()).unwrap(), "b");
        assert_eq!(
            number_or_name("custom-model", ["a"].into_iter()).unwrap(),
            "custom-model"
        );
        assert!(number_or_name("0", ["a"].into_iter()).is_err());
        assert!(number_or_name("20", ["a"].into_iter()).is_err());
    }
}
