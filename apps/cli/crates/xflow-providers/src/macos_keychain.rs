//! Migration only: reads and deletes keys that older builds saved in the login
//! Keychain. ponytail: migration only; drop keyring and this file in v0.2.
use std::process::{Command, Stdio};

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecKeychainSetUserInteractionAllowed(allowed: u8) -> i32;
}

/// With `interactive` false, a read that needs the user's approval fails at once
/// (errSecAuthFailed, observed on macOS 26) instead of showing a dialog. The call
/// is deprecated but still honoured; it's process-wide, so callers serialize.
pub fn read(service: &str, account: &str, interactive: bool) -> keyring::Result<String> {
    unsafe { SecKeychainSetUserInteractionAllowed(interactive as u8) };
    let value = keyring::Entry::new(service, account).and_then(|e| e.get_password());
    unsafe { SecKeychainSetUserInteractionAllowed(1) };
    value
}

/// Apple's `security` deletes an item another build wrote without asking, where
/// SecItemDelete from this binary is refused (errSecInvalidOwnerEdit, -25244).
/// Ok once the item is gone: deleted (0) or not found (44).
pub fn delete(service: &str, account: &str) -> anyhow::Result<()> {
    let status = Command::new("/usr/bin/security")
        .args(["delete-generic-password", "-s", service, "-a", account])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    match status.code() {
        Some(0 | 44) => Ok(()),
        _ => anyhow::bail!("Keychain item {service}/{account} was not deleted ({status})"),
    }
}
