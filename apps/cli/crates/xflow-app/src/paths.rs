use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub fn config_path() -> Result<PathBuf> {
    Ok(env_path("XDG_CONFIG_HOME")
        .or_else(|| home().map(|p| p.join(".config")))
        .context("HOME or XDG_CONFIG_HOME is required")?
        .join("xflow/config.toml"))
}
pub fn data_dir() -> Result<PathBuf> {
    Ok(env_path("XDG_DATA_HOME")
        .or_else(|| home().map(|p| p.join(".local/share")))
        .context("HOME or XDG_DATA_HOME is required")?
        .join("xflow"))
}
fn home() -> Option<PathBuf> {
    env_path("HOME")
}
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
pub fn runtime_dir() -> Result<PathBuf> {
    let path = if let Some(base) = env_path("XDG_RUNTIME_DIR") {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&base)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            bail!("unsafe XDG_RUNTIME_DIR");
        }
        base.join("xflow")
    } else {
        PathBuf::from(format!("/tmp/xflow-{}", unsafe { libc::geteuid() }))
    };
    private_dir(&path)?;
    Ok(path)
}

#[cfg(unix)]
pub fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    // Validate the final component before touching permissions; never follow a symlink.
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        bail!("unsafe private directory {}", path.display());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}
