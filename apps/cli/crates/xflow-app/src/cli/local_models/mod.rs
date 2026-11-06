use super::{args, ipc, output, Error, Output, Result};
use serde_json::{json, Value};
use xflow_app::local_models::{self as lm, install, Host};
use xflow_providers::on_device;

/// Refuse to select on-device speech this machine cannot run or has not installed.
pub fn ready_to_use() -> Result<()> {
    if !lm::facts().supported {
        return Err(Error::new(
            1,
            on_device::UNSUPPORTED,
            "Keep an API provider: xflow providers list",
        ));
    }
    if !lm::installed() {
        return Err(Error::new(
            1,
            "The on-device model is not installed",
            format!("Run xflow models install {}", on_device::MODEL_ID),
        ));
    }
    Ok(())
}

pub fn manage(action: &args::ModelAction, out: &Output) -> Result<()> {
    let (args::ModelAction::Install { model, .. } | args::ModelAction::Remove { model }) = action;
    if model != on_device::MODEL_ID {
        return Err(Error::new(
            2,
            format!("Unknown on-device model: {model}"),
            format!("The available model is {}", on_device::MODEL_ID),
        ));
    }
    let models = on_device::models_dir().map_err(Error::config)?;
    match action {
        args::ModelAction::Install { from, .. } => {
            if !on_device::SUPPORTED {
                return Err(Error::new(
                    1,
                    on_device::UNSUPPORTED,
                    "Keep an API provider: xflow providers list",
                ));
            }
            let artifact = lm::artifact();
            let source = match from {
                Some(dir) => install::Source::Dir(dir),
                None => install::Source::Http(install::BASE_URL),
            };
            let started = std::time::Instant::now();
            let meter = |done: u64, total: u64| {
                let rate = done as f64 / started.elapsed().as_secs_f64().max(0.001) / 1e6;
                eprint!("\r{model}: {done} / {total} bytes ({rate:.1} MB/s)   ");
            };
            let progress = !out.json && !out.quiet;
            let fresh = ipc::block_on(async {
                let result = install::install(
                    &artifact,
                    source,
                    &models,
                    progress.then_some(&meter as &dyn Fn(u64, u64)),
                )
                .await;
                if progress {
                    eprintln!();
                }
                result.map_err(|e| {
                    Error::new(1, format!("{e:#}"), "Run the same command again to resume")
                })
            })?;
            let dir = models.join(on_device::MODEL_ID);
            out.data(
                &json!({"model":model,"installed":true,"path":dir.to_string_lossy(),"changed":fresh}),
                &if fresh {
                    format!("Installed {model} in {}. Use it: xflow providers use on-device", dir.display())
                } else {
                    format!("{model} is already installed")
                },
            )?;
        }
        args::ModelAction::Remove { .. } => {
            let removed = install::remove(&models).map_err(Error::config)?;
            out.data(
                &json!({"model":model,"removed":removed}),
                &if removed {
                    format!("Removed {model}; keys, settings and history are unchanged")
                } else {
                    format!("{model} is not installed")
                },
            )?;
        }
    }
    Ok(())
}

fn report(host: &Host) -> Value {
    let catalog = lm::catalog();
    let requirements = serde_json::from_value(catalog["models"][0]["requirements"].clone())
        .expect("catalog requirements");
    let model_dir = on_device::model_dir().ok();
    let helper = on_device::helper_path();
    json!({"schema_version":1,"host":host,"catalog":catalog,"fit":lm::fit(host, &requirements),
        "install":{"installed":lm::installed(),
            "model_dir":model_dir.map(|p| p.to_string_lossy().into_owned()),
            "helper":helper.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|e| format!("{e:#}"))}})
}

fn human(report: &Value) -> String {
    let host = &report["host"];
    let model = &report["catalog"]["models"][0];
    let display = |v: &Value| {
        v.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string())
    };
    let mut rows = Vec::new();
    for (key, label) in [
        ("platform", "Platform"),
        ("os_version", "OS version"),
        ("native_isa", "Native architecture"),
        ("process_isa", "Process architecture"),
        ("rosetta", "Rosetta"),
        ("chip", "Chip"),
        ("physical_cores", "Physical cores"),
        ("logical_cores", "Logical cores"),
        ("performance_cores", "Performance cores"),
        ("installed_ram_bytes", "Installed RAM (bytes)"),
        ("install_path", "Intended model directory"),
        ("disk_checked_path", "Disk checked at"),
        ("available_disk_bytes", "Available disk (bytes)"),
        ("cpu_available", "CPU available"),
        ("gpu_available", "GPU available"),
        ("neural_engine_available", "Neural Engine available"),
    ] {
        rows.push(vec![label.into(), display(&host[key])]);
    }
    let languages = model["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| format!("{} ({})", display(&l["name"]), display(&l["code"])))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Local model catalog (bundled, offline)\n{}\n\n{} [{}]\nStatus: {}; {}\nInstalled: {}\nHelper: {}\nHardware: {}\nMemory fit: unverified\nLanguages (25): {}\n{}\nLicense: {} — {}\n{}\n{}\nSource: {}\nAccuracy: NVIDIA base-model English average WER 6.34%, Open ASR Leaderboard.\nDatasets: {}. Punctuation and capitalization removed.\nSource checked: {}; evaluation date: unknown.\n{}\nComparable converted-runtime accuracy: unknown (unknown scores sort last).\nAccuracy source: {}\nDownload: {} bytes (model files only). {}\nRequirements: macOS 14+, Apple Silicon; RAM requirement unknown.\n{}\nDisk allowance: {} bytes. {}\nArtifact: {} @ {}\n{}\nPlanned runtime: {} @ {} ({}, not installed)\nRuntime license: {}\nWarnings:\n{}\nInstall: xflow models install {} [--from DIR]. Use: xflow providers use on-device\n",
        output::table(&["HOST", "VALUE"], &rows), display(&model["name"]), display(&model["id"]),
        display(&report["fit"]["readiness"]).replace('_', " "), display(&report["fit"]["reason"]),
        if report["install"]["installed"] == true { "yes" } else { "no" }, display(&report["install"]["helper"]), display(&report["fit"]["hardware"]), languages,
        display(&model["language_warning"]), display(&model["license"]["id"]), display(&model["license"]["url"]),
        display(&model["license"]["attribution"]), display(&model["license"]["modifications"]), display(&model["license"]["source"]),
        model["accuracy"]["datasets"].as_array().unwrap().iter().map(display).collect::<Vec<_>>().join(", "),
        display(&model["accuracy"]["source_checked_date"]), display(&model["accuracy"]["scope"]), display(&model["accuracy"]["source"]),
        model["artifact"]["download_bytes"], display(&model["artifact"]["size_note"]), display(&model["requirements"]["compute"]),
        model["requirements"]["advisory_free_disk_bytes"], display(&model["requirements"]["disk_note"]),
        display(&model["artifact"]["repository"]), display(&model["artifact"]["revision"]), display(&model["artifact"]["variant"]),
        display(&model["planned_runtime"]["name"]), display(&model["planned_runtime"]["revision"]), display(&model["planned_runtime"]["license"]), display(&model["planned_runtime"]["license_source"]),
        report["fit"]["warnings"].as_array().unwrap().iter().map(|s| format!("- {}", display(s))).collect::<Vec<_>>().join("\n"), display(&model["id"]))
}

pub fn run(out: &Output) -> Result<()> {
    let install_path = on_device::model_dir().ok();
    let report = report(&lm::probe(install_path));
    out.data(&report, &human(&report))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_and_text_explain_unknowns_and_do_not_imply_readiness() {
        let value = report(&Host {
            platform: "linux",
            ..Host::default()
        });
        let text = human(&value);
        assert_eq!(value["host"]["installed_ram_bytes"], "unknown");
        assert_eq!(value["fit"]["hardware"], "unsupported");
        for expected in [
            "Status: not ready",
            "Memory fit: unverified",
            "unknown",
            "Hindi",
            "Japanese",
            "6.34%",
            "base model",
            "not this quantized",
            "483105645",
            "CC-BY-4.0",
            "Apache-2.0",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
    }
}
