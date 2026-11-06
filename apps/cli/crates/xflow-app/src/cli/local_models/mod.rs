mod host;
mod install;

use super::{args, ipc, output, Error, Output, Result};
use host::Host;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use xflow_providers::on_device;

#[derive(Deserialize)]
struct Requirements {
    platform: String,
    native_isa: String,
    minimum_os_major: u32,
    advisory_free_disk_bytes: u64,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Compatibility {
    HardwareEligible,
    Incompatible,
    NotReady,
    Unsupported,
    Unknown,
}

#[derive(Serialize)]
struct Fit {
    hardware: Compatibility,
    readiness: &'static str,
    memory_fit: &'static str,
    reason: &'static str,
    warnings: Vec<String>,
}

fn fit(host: &Host, requirements: &Requirements) -> Fit {
    use Compatibility::*;
    let os_major = host
        .os_version
        .as_ref()
        .and_then(|s| s.split('.').next()?.parse::<u32>().ok());
    let (hardware, reason) = if host.platform != requirements.platform {
        (
            Unsupported,
            "Managed local transcription is not supported on this platform.",
        )
    } else if os_major.is_some_and(|v| v < requirements.minimum_os_major) {
        (Incompatible, "This model requires macOS 14 or newer.")
    } else {
        match host.native_isa.as_deref() {
            Some("x86_64") => (NotReady, "Intel local transcription is not ready: the CPU helper has not been validated."),
            Some(isa) if isa != requirements.native_isa => (Incompatible, "This model requires Apple Silicon (64-bit Arm)."),
            Some(_) if os_major.is_some() => (HardwareEligible, "Apple Silicon and macOS requirements met; memory fit is unverified and the runtime is not installed."),
            _ => (Unknown, "Hardware compatibility is unknown: native architecture or macOS version could not be read."),
        }
    };
    let mut warnings = Vec::new();
    match host.available_disk_bytes {
        Some(bytes) if bytes < requirements.advisory_free_disk_bytes => warnings.push(format!(
            "Low disk space: {bytes} bytes available; allow at least {} bytes for model staging. Full installation space remains unverified.", requirements.advisory_free_disk_bytes)),
        None => warnings.push("Free disk space on the intended install volume is unknown.".into()),
        _ => (),
    }
    if host.installed_ram_bytes.is_none() {
        warnings.push("Installed RAM is unknown.".into());
    }
    warnings.push("Total system memory needed by local transcription has not been qualified. No smooth-fit or speed estimate is available.".into());
    Fit {
        readiness: if hardware == HardwareEligible && on_device::SUPPORTED && ready() {
            "ready"
        } else {
            "not_ready"
        },
        hardware,
        memory_fit: "unverified",
        reason,
        warnings,
    }
}

fn catalog() -> Value {
    let mut catalog: Value =
        serde_json::from_str(include_str!("catalog.json")).expect("bundled catalog");
    // Only scores explicitly curated as comparable may rank models; base-model WER is not that score.
    catalog["models"]
        .as_array_mut()
        .expect("catalog models")
        .sort_by(compare_accuracy);
    catalog
}

fn compare_accuracy(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (
        a["accuracy"]["comparable_wer_percent"].as_f64(),
        b["accuracy"]["comparable_wer_percent"].as_f64(),
    ) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

fn requirements() -> Requirements {
    serde_json::from_value(catalog()["models"][0]["requirements"].clone())
        .expect("catalog requirements")
}
fn artifact() -> install::Artifact {
    serde_json::from_value(catalog()["models"][0]["artifact"].clone()).expect("catalog artifact")
}
fn installed() -> bool {
    on_device::model_dir().is_ok_and(|dir| on_device::installed(&dir))
}
/// Installed, and the helper is where xflow looks for it.
fn ready() -> bool {
    installed() && on_device::helper_path().is_ok()
}

/// Refuse to select on-device speech this machine cannot run or has not installed.
pub fn ready_to_use() -> Result<()> {
    let hardware = fit(&host::probe(None), &requirements()).hardware;
    if !on_device::SUPPORTED || matches!(hardware, Compatibility::Incompatible) {
        return Err(Error::new(
            1,
            on_device::UNSUPPORTED,
            "Keep an API provider: xflow providers list",
        ));
    }
    if !installed() {
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
            let artifact = artifact();
            let source = match from {
                Some(dir) => install::Source::Dir(dir),
                None => install::Source::Http(install::BASE_URL),
            };
            let progress = !out.json && !out.quiet;
            let fresh = ipc::block_on(async {
                install::install(&artifact, source, &models, progress)
                    .await
                    .map_err(|e| {
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
    let catalog = catalog();
    let requirements = serde_json::from_value(catalog["models"][0]["requirements"].clone())
        .expect("catalog requirements");
    let model_dir = on_device::model_dir().ok();
    let helper = on_device::helper_path();
    json!({"schema_version":1,"host":host,"catalog":catalog,"fit":fit(host, &requirements),
        "install":{"installed":installed(),
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
    let report = report(&host::probe(install_path));
    out.data(&report, &human(&report))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn requirements() -> Requirements {
        serde_json::from_value(catalog()["models"][0]["requirements"].clone()).unwrap()
    }
    fn apple() -> Host {
        Host {
            platform: "macos",
            process_isa: "aarch64",
            native_isa: Some("aarch64".into()),
            os_version: Some("14.0".into()),
            installed_ram_bytes: Some(8 << 30),
            available_disk_bytes: Some(10 << 30),
            ..Host::default()
        }
    }
    #[test]
    fn compatibility_uses_facts_without_claiming_memory_fit() {
        let r = requirements();
        let mut host = apple();
        assert_eq!(fit(&host, &r).hardware, Compatibility::HardwareEligible);
        host.chip = Some("unlisted future chip".into());
        host.installed_ram_bytes = Some(1);
        assert_eq!(fit(&host, &r).hardware, Compatibility::HardwareEligible);
        assert_eq!(fit(&host, &r).memory_fit, "unverified");
        assert_eq!(fit(&host, &r).readiness, "not_ready");
        host.os_version = Some("13.9".into());
        assert_eq!(fit(&host, &r).hardware, Compatibility::Incompatible);
        host.os_version = Some("26.7.1".into());
        host.native_isa = Some("x86_64".into());
        assert_eq!(fit(&host, &r).hardware, Compatibility::NotReady);
        host.native_isa = Some("i386".into());
        assert_eq!(fit(&host, &r).hardware, Compatibility::Incompatible);
        host.native_isa = None;
        assert_eq!(fit(&host, &r).hardware, Compatibility::Unknown);
        host.native_isa = Some("aarch64".into());
        for version in [None, Some("unknown".into())] {
            host.os_version = version;
            assert_eq!(fit(&host, &r).hardware, Compatibility::Unknown);
        }
        host.platform = "linux";
        assert_eq!(fit(&host, &r).hardware, Compatibility::Unsupported);
    }
    #[test]
    fn rosetta_and_low_disk_are_advisory() {
        let r = requirements();
        let mut host = apple();
        host.process_isa = "x86_64";
        host.rosetta = Some(true);
        host.available_disk_bytes = Some(0);
        let result = fit(&host, &r);
        assert_eq!(result.hardware, Compatibility::HardwareEligible);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("Low disk space: 0 bytes")));
        host.available_disk_bytes = None;
        host.installed_ram_bytes = None;
        let result = fit(&host, &r);
        assert_eq!(result.hardware, Compatibility::HardwareEligible);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("disk space") && w.contains("unknown")));
        assert!(result
            .warnings
            .iter()
            .any(|w| w == "Installed RAM is unknown."));
    }
    #[test]
    fn catalog_has_exact_languages_pins_safe_complete_manifest_and_qualified_accuracy() {
        let c = catalog();
        assert_eq!(c["models"].as_array().unwrap().len(), 1);
        let model = &c["models"][0];
        let languages: Vec<_> = model["languages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["code"].as_str().unwrap())
            .collect();
        assert_eq!(
            languages,
            [
                "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv",
                "lt", "mt", "pl", "pt", "ro", "ru", "sk", "sl", "es", "sv", "uk"
            ]
        );
        assert!(!languages.contains(&"hi") && !languages.contains(&"ja"));
        assert_eq!(languages, on_device::LANGUAGES);
        assert_eq!(
            on_device::model_dir().unwrap(),
            std::path::absolute(xflow_app::paths::data_dir().unwrap())
                .unwrap()
                .join("models")
                .join(on_device::MODEL_ID)
        );
        assert_eq!(
            model["artifact"]["revision"],
            "7dd20fe6b1797d35f5e3307e8b1732d9a178edfe"
        );
        assert_eq!(
            model["planned_runtime"]["revision"],
            "ee78635d23d52379117554ba72358b65a0612447"
        );
        assert_eq!(model["license"]["id"], "CC-BY-4.0");
        assert_eq!(model["planned_runtime"]["license"], "Apache-2.0");
        assert_eq!(model["accuracy"]["vendor_english_wer_percent"], 6.34);
        assert_eq!(model["accuracy"]["comparable_wer_percent"], "unknown");
        let files = model["artifact"]["files"].as_array().unwrap();
        let mut paths = std::collections::BTreeSet::new();
        assert_eq!(files.len(), 21);
        for file in files {
            let path = file["path"].as_str().unwrap();
            assert!(paths.insert(path));
            assert!(!path.starts_with('/') && !path.contains('\\'));
            assert!(path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."));
            assert!(model["architecture"]["roles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| path.split('/').next().unwrap() == r.as_str().unwrap()));
            let hash = file["sha256"].as_str().unwrap();
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(file["bytes"].as_u64().unwrap() > 0);
        }
        assert_eq!(
            files
                .iter()
                .map(|f| f["bytes"].as_u64().unwrap())
                .sum::<u64>(),
            483_105_645
        );
        assert_eq!(model["artifact"]["download_bytes"], 483_105_645u64);
        assert_eq!(
            requirements().advisory_free_disk_bytes,
            2 * 483_105_645 + (1 << 30)
        );
        assert!(!paths
            .iter()
            .any(|p| p.contains("JointDecision.mlmodelc") || p.contains("int4")));
    }
    #[test]
    fn comparable_accuracy_sorts_unknown_last_without_using_vendor_wer() {
        let mut models = [
            json!({"accuracy":{"vendor_english_wer_percent":0.1,"comparable_wer_percent":"unknown"}}),
            json!({"accuracy":{"comparable_wer_percent":7.0}}),
            json!({"accuracy":{"comparable_wer_percent":3.0}}),
        ];
        models.sort_by(compare_accuracy);
        assert_eq!(models[0]["accuracy"]["comparable_wer_percent"], 3.0);
        assert_eq!(models[1]["accuracy"]["comparable_wer_percent"], 7.0);
        assert_eq!(models[2]["accuracy"]["comparable_wer_percent"], "unknown");
    }
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
