//! Keychain items whose access list trusts both xflow binaries: a key saved by
//! `xflow` is read by `xflowd` without macOS asking "Always Allow". Trust follows
//! each binary's designated requirement, so it survives rebuilds signed by one
//! certificate (see README, xflow-dev).
use anyhow::{bail, Result};
use std::{
    ffi::{c_char, c_void, CString},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    ptr,
};

type Ref = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: u8;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFArrayCreate(alloc: Ref, values: *const Ref, count: isize, callbacks: Ref) -> Ref;
    fn CFDictionaryCreate(
        alloc: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    fn CFStringCreateWithBytes(
        alloc: Ref,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFDataCreate(alloc: Ref, bytes: *const u8, count: isize) -> Ref;
    fn CFRelease(value: Ref);
}
#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecClass: Ref;
    static kSecClassGenericPassword: Ref;
    static kSecAttrService: Ref;
    static kSecAttrAccount: Ref;
    static kSecValueData: Ref;
    static kSecAttrAccess: Ref;
    fn SecTrustedApplicationCreateFromPath(path: *const c_char, app: *mut Ref) -> i32;
    fn SecAccessCreate(descriptor: Ref, trusted: Ref, access: *mut Ref) -> i32;
    fn SecItemAdd(attributes: Ref, result: *mut Ref) -> i32;
}

/// Releases every CF object created for one call.
struct Owned(Vec<Ref>);
impl Owned {
    fn keep(&mut self, value: Ref) -> Result<Ref> {
        if value.is_null() {
            bail!("Keychain request could not be built");
        }
        self.0.push(value);
        Ok(value)
    }
    fn string(&mut self, s: &str) -> Result<Ref> {
        // kCFStringEncodingUTF8
        self.keep(unsafe {
            CFStringCreateWithBytes(ptr::null(), s.as_ptr(), s.len() as isize, 0x0800_0100, 0)
        })
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        for value in self.0.drain(..).rev() {
            unsafe { CFRelease(value) }
        }
    }
}

/// `xflow` and `xflowd` beside the running binary, plus the running binary itself.
pub fn trusted_apps() -> Vec<PathBuf> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .unwrap_or_default();
    let mut apps: Vec<_> = ["xflow", "xflowd"]
        .iter()
        .map(|name| exe.with_file_name(name))
        .filter(|path| path.is_file())
        .collect();
    if !apps.contains(&exe) {
        apps.push(exe);
    }
    apps
}

/// Add a generic password readable without a prompt by exactly `apps`.
/// Fails with errSecDuplicateItem (-25299) when the item exists; delete it first.
pub fn add(service: &str, account: &str, secret: &[u8], apps: &[PathBuf]) -> Result<()> {
    let mut owned = Owned(vec![]);
    let mut trusted = vec![];
    for path in apps {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            continue;
        };
        let mut app = ptr::null();
        if unsafe { SecTrustedApplicationCreateFromPath(path.as_ptr(), &mut app) } == 0 {
            trusted.push(owned.keep(app)?);
        }
    }
    // An empty list would mean "always ask"; a null one "anyone": refuse both.
    if trusted.is_empty() {
        bail!("no xflow binary could be trusted for the Keychain item");
    }
    let list = owned.keep(unsafe {
        CFArrayCreate(
            ptr::null(),
            trusted.as_ptr(),
            trusted.len() as isize,
            &kCFTypeArrayCallBacks as *const u8 as Ref,
        )
    })?;
    let label = owned.string(service)?;
    let mut access = ptr::null();
    let status = unsafe { SecAccessCreate(label, list, &mut access) };
    if status != 0 {
        bail!("Keychain access list could not be created ({status})");
    }
    let access = owned.keep(access)?;
    let service = owned.string(service)?;
    let account = owned.string(account)?;
    let data =
        owned.keep(unsafe { CFDataCreate(ptr::null(), secret.as_ptr(), secret.len() as isize) })?;
    let (keys, values) = unsafe {
        (
            [
                kSecClass,
                kSecAttrService,
                kSecAttrAccount,
                kSecValueData,
                kSecAttrAccess,
            ],
            [kSecClassGenericPassword, service, account, data, access],
        )
    };
    let query = owned.keep(unsafe {
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            &kCFTypeDictionaryKeyCallBacks as *const u8 as Ref,
            &kCFTypeDictionaryValueCallBacks as *const u8 as Ref,
        )
    })?;
    let status = unsafe { SecItemAdd(query, ptr::null_mut()) };
    if status != 0 {
        bail!("Keychain item could not be added ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Touches the login Keychain: run by hand with `--ignored`.
    #[test]
    #[ignore]
    fn shared_item_lists_every_trusted_binary() {
        let (service, account) = ("xflow-acl-selftest", "probe");
        let delete = || {
            std::process::Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", service, "-a", account])
                .output()
                .unwrap()
        };
        delete();
        // Another trusted binary reads it without a dialog (it would block here).
        let apps = [super::trusted_apps(), vec!["/usr/bin/security".into()]].concat();
        super::add(service, account, b"secret", &apps).unwrap();
        let read = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", service, "-a", account, "-w"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&read.stdout).trim(), "secret");
        assert!(delete().status.success());
    }
}
