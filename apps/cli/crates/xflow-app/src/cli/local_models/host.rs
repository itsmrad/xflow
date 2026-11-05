use serde::Serialize;
#[cfg(any(target_os = "macos", test))]
use std::path::Path;
use std::path::PathBuf;

#[derive(Default, Debug, Serialize)]
pub struct Host {
    pub platform: &'static str,
    pub process_isa: &'static str,
    #[serde(serialize_with = "unknown")]
    pub native_isa: Option<String>,
    #[serde(serialize_with = "unknown")]
    pub rosetta: Option<bool>,
    #[serde(serialize_with = "unknown")]
    pub chip: Option<String>,
    #[serde(serialize_with = "unknown")]
    pub physical_cores: Option<u64>,
    #[serde(serialize_with = "unknown")]
    pub logical_cores: Option<u64>,
    #[serde(serialize_with = "unknown")]
    pub performance_cores: Option<u64>,
    #[serde(serialize_with = "unknown")]
    pub installed_ram_bytes: Option<u64>,
    #[serde(serialize_with = "unknown")]
    pub os_version: Option<String>,
    #[serde(serialize_with = "unknown")]
    pub install_path: Option<PathBuf>,
    #[serde(serialize_with = "unknown")]
    pub disk_checked_path: Option<PathBuf>,
    #[serde(serialize_with = "unknown")]
    pub available_disk_bytes: Option<u64>,
    #[serde(serialize_with = "unknown")]
    pub cpu_available: Option<bool>,
    #[serde(serialize_with = "unknown")]
    pub gpu_available: Option<bool>,
    #[serde(serialize_with = "unknown")]
    pub neural_engine_available: Option<bool>,
}

fn unknown<T: Serialize, S: serde::Serializer>(value: &Option<T>, s: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => value.serialize(s),
        None => s.serialize_str("unknown"),
    }
}

pub fn probe(install_path: Option<PathBuf>) -> Host {
    let host = Host {
        platform: std::env::consts::OS,
        process_isa: std::env::consts::ARCH,
        install_path,
        ..Host::default()
    };
    #[cfg(target_os = "macos")]
    return macos::probe(host);
    #[cfg(not(target_os = "macos"))]
    host
}

// Read the volume of the nearest existing ancestor without creating an install directory.
#[cfg(any(target_os = "macos", test))]
fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut path = path;
    loop {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return path.canonicalize().ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => path = path.parent()?,
            Err(_) => return None,
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::{CStr, CString};

    fn sysctl(name: &CStr) -> Option<Vec<u8>> {
        let mut bytes = [0u8; 512];
        let mut len = bytes.len();
        // SAFETY: both pointers reference writable buffers of the declared size; no new value is set.
        let result = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (result == 0 && len <= bytes.len()).then(|| bytes[..len].to_vec())
    }
    fn text(name: &CStr) -> Option<String> {
        let bytes = sysctl(name)?;
        let text = String::from_utf8(bytes.strip_suffix(&[0]).unwrap_or(&bytes).to_vec()).ok()?;
        (!text.is_empty()).then_some(text)
    }
    fn number(name: &CStr) -> Option<u64> {
        let bytes = sysctl(name)?;
        match bytes.len() {
            4 => Some(u32::from_ne_bytes(bytes.try_into().ok()?) as u64),
            8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
            _ => None,
        }
    }
    fn positive(name: &CStr) -> Option<u64> {
        number(name).filter(|&n| n > 0)
    }
    pub(super) fn probe(mut host: Host) -> Host {
        let arm = number(c"hw.optional.arm64");
        let translated = number(c"sysctl.proc_translated").map(|n| n == 1);
        host.native_isa = match (arm, translated) {
            (Some(1), _) | (_, Some(true)) => Some("aarch64".into()),
            _ => {
                let mut uts = std::mem::MaybeUninit::<libc::utsname>::uninit();
                // SAFETY: uname initializes uts on success, including a NUL-terminated machine field.
                if unsafe { libc::uname(uts.as_mut_ptr()) } == 0 {
                    Some(
                        unsafe { CStr::from_ptr(uts.assume_init().machine.as_ptr()) }
                            .to_string_lossy()
                            .into_owned(),
                    )
                } else {
                    None
                }
            }
        };
        if host.native_isa.as_deref() == Some("arm64") {
            host.native_isa = Some("aarch64".into());
        }
        // uname can describe the translated process; absence of both ISA facts cannot prove Intel.
        if host.native_isa.as_deref() == Some("x86_64") && arm.is_none() && translated.is_none() {
            host.native_isa = None;
        }
        host.rosetta = if host.process_isa == "aarch64" {
            Some(false)
        } else {
            translated
        };
        host.chip = text(c"machdep.cpu.brand_string");
        host.physical_cores = positive(c"hw.physicalcpu");
        host.logical_cores = positive(c"hw.logicalcpu");
        if text(c"hw.perflevel0.name").as_deref() == Some("Performance") {
            host.performance_cores = positive(c"hw.perflevel0.physicalcpu");
        }
        host.installed_ram_bytes = positive(c"hw.memsize");
        host.os_version = text(c"kern.osproductversion");
        host.cpu_available = host.logical_cores.map(|n| n > 0);
        // Accelerator enumeration would add inference-framework linkage; availability stays unknown.
        if let Some(path) = host.install_path.as_deref().and_then(existing_ancestor) {
            if let Ok(cpath) = CString::new(path.as_os_str().as_encoded_bytes()) {
                let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
                // SAFETY: cpath is NUL terminated and stat is initialized only on successful statvfs.
                if unsafe { libc::statvfs(cpath.as_ptr(), stat.as_mut_ptr()) } == 0 {
                    let stat = unsafe { stat.assume_init() };
                    host.available_disk_bytes = u64::from(stat.f_bavail).checked_mul(stat.f_frsize);
                    host.disk_checked_path = Some(path);
                }
            }
        }
        host
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_facts_stay_unknown_and_zero_disk_is_known() {
        let mut host = Host::default();
        let json = serde_json::to_value(&host).unwrap();
        assert_eq!(json["installed_ram_bytes"], "unknown");
        assert_eq!(json["neural_engine_available"], "unknown");
        host.available_disk_bytes = Some(0);
        assert_eq!(
            serde_json::to_value(host).unwrap()["available_disk_bytes"],
            0
        );
    }
    #[test]
    fn resolves_future_install_volume_without_writing() {
        let root = tempfile::tempdir().unwrap();
        let future = root.path().join("models/future");
        assert_eq!(existing_ancestor(&future), root.path().canonicalize().ok());
        assert!(!future.exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("missing"), root.path().join("broken"))
                .unwrap();
            assert_eq!(existing_ancestor(&root.path().join("broken")), None);
        }
    }
}
