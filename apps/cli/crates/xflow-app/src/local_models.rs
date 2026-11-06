//! The bundled on-device model catalog, this machine's fit, and the installer.
//! Shared by `xflow models`, `xflow providers` and the TUI.
mod host;
pub mod install;

pub use host::{probe, Host};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xflow_providers::on_device;

#[derive(Deserialize)]
pub struct Requirements {
    pub platform: String,
    pub native_isa: String,
    pub minimum_os_major: u32,
    pub advisory_free_disk_bytes: u64,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Compatibility {
    HardwareEligible,
    Incompatible,
    NotReady,
    Unsupported,
    Unknown,
}

#[derive(Serialize)]
pub struct Fit {
    pub hardware: Compatibility,
    pub readiness: &'static str,
    pub memory_fit: &'static str,
    pub reason: &'static str,
    pub warnings: Vec<String>,
}

pub fn fit(host: &Host, requirements: &Requirements) -> Fit {
    use Compatibility::*;
    let os_major = host
        .os_version
        .as_ref()
        .and_then(|s| s.split('.').next()?.parse::<u32>().ok());
    let (hardware, reason) = if host.platform != requirements.platform {
        let reason = match host.platform {
            "linux" => "Not supported yet on Linux. It needs an Apple Silicon Mac with macOS 14 or newer.",
            "windows" => "Not supported yet on Windows. It needs an Apple Silicon Mac with macOS 14 or newer.",
            _ => "Not supported yet on this system. It needs an Apple Silicon Mac with macOS 14 or newer.",
        };
        (Unsupported, reason)
    } else if os_major.is_some_and(|v| v < requirements.minimum_os_major) {
        (Incompatible, "Needs macOS 14 or newer. Update macOS to use it.")
    } else {
        match host.native_isa.as_deref() {
            Some("x86_64") => (NotReady, "Not supported yet on Intel Macs. It needs Apple Silicon and macOS 14 or newer."),
            Some(isa) if isa != requirements.native_isa => (Incompatible, "Needs an Apple Silicon Mac."),
            Some(_) if os_major.is_some() => (HardwareEligible, "This Mac can run it: Apple Silicon and macOS 14 or newer. Memory use is not measured yet."),
            _ => (Unknown, "Couldn't read this Mac's chip or macOS version. It needs Apple Silicon and macOS 14 or newer."),
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

pub fn catalog() -> Value {
    let mut catalog: Value =
        serde_json::from_str(include_str!("local_models/catalog.json")).expect("bundled catalog");
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

pub fn requirements() -> Requirements {
    serde_json::from_value(catalog()["models"][0]["requirements"].clone())
        .expect("catalog requirements")
}
pub fn artifact() -> install::Artifact {
    serde_json::from_value(catalog()["models"][0]["artifact"].clone()).expect("catalog artifact")
}
pub fn installed() -> bool {
    on_device::model_dir().is_ok_and(|dir| on_device::installed(&dir))
}
/// Installed, and the helper is where xflow looks for it.
pub fn ready() -> bool {
    installed() && on_device::helper_path().is_ok()
}

/// What setup, Settings and `xflow providers info on-device` show about on-device speech.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub name: String,
    pub languages: Vec<String>,
    /// Hindi, Hindi-English and Japanese are unsupported.
    pub warning: String,
    pub download_bytes: u64,
    /// "CC BY 4.0 · NVIDIA: …. Core ML conversion by FluidInference."
    pub license: String,
    /// Plain fit reason from this machine's facts.
    pub fit: String,
    /// This build and machine can run it (Apple Silicon macOS 14+).
    pub supported: bool,
    pub installed: bool,
}
impl Facts {
    pub fn lines(&self) -> Vec<String> {
        vec![
            format!("Model: {}", self.name),
            format!("Download: {}", size(self.download_bytes)),
            format!(
                "Languages ({}): {}",
                self.languages.len(),
                self.languages.join(", ")
            ),
            format!("Not supported: {}", self.warning),
            format!("License: {}", self.license),
            format!("This machine: {}", self.fit),
        ]
    }
}
/// "484 MB".
pub fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes.div_ceil(1_000_000))
    }
}
pub fn facts() -> Facts {
    facts_for(&probe(on_device::model_dir().ok()), installed())
}
pub fn facts_for(host: &Host, installed: bool) -> Facts {
    let catalog = catalog();
    let model = &catalog["models"][0];
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let fit = fit(host, &requirements());
    Facts {
        name: text(&model["name"]),
        languages: model["languages"]
            .as_array()
            .expect("catalog languages")
            .iter()
            .map(|l| text(&l["name"]))
            .collect(),
        warning: text(&model["language_warning"]),
        download_bytes: model["artifact"]["download_bytes"].as_u64().unwrap_or(0),
        license: format!(
            "{} · {}",
            text(&model["license"]["id"]).replace('-', " "),
            text(&model["license"]["attribution"])
        ),
        supported: on_device::SUPPORTED
            && host.platform == "macos"
            && !matches!(
                fit.hardware,
                Compatibility::Incompatible | Compatibility::Unsupported | Compatibility::NotReady
            ),
        fit: fit.reason.into(),
        installed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
            std::path::absolute(crate::paths::data_dir().unwrap())
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
    fn facts_name_languages_license_and_plain_fit() {
        let facts = facts_for(&apple(), false);
        assert_eq!(facts.name, "NVIDIA Parakeet TDT 0.6B v3");
        assert_eq!(facts.languages.len(), 25);
        assert!(facts.warning.contains("Hindi") && facts.warning.contains("Japanese"));
        assert_eq!(size(facts.download_bytes), "484 MB");
        assert!(facts.license.starts_with("CC BY 4.0 · NVIDIA"), "{}", facts.license);
        assert!(facts.license.contains("FluidInference"));
        assert_eq!(facts.supported, on_device::SUPPORTED);
        let linux = facts_for(&Host { platform: "linux", ..Host::default() }, false);
        assert!(!linux.supported);
        assert!(linux.fit.starts_with("Not supported yet on Linux"));
        let mut intel = apple();
        intel.native_isa = Some("x86_64".into());
        let intel = facts_for(&intel, false);
        assert!(!intel.supported && intel.fit.contains("Intel"));
        assert!(facts.lines().iter().any(|l| l.starts_with("Languages (25): Bulgarian")));
    }
}
