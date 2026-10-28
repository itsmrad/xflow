//! xflow's own credential store. Provider keys are sealed with ChaCha20-Poly1305
//! (the provider id is the AAD, so entries can't be swapped) in
//! `$XDG_CONFIG_HOME/xflow/credentials`, under 32 random bytes kept apart in
//! `$XDG_DATA_HOME/xflow/keystore.key`. Both are 0600 in 0700 directories that
//! are opened once (never through a symlink) and used by descriptor.
//!
//! Threat model: encrypted at rest against casual disclosure of the config dir
//! (`cat`, a copied, synced or committed `~/.config`, other OS users). Not
//! protection from malware running as you, nor from a backup holding both files.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::DirBuilderExt, fs::MetadataExt, fs::PermissionsExt},
    },
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAX_FILE: u64 = 1 << 20;
const CREDENTIALS: &str = "credentials";
const KEY: &str = "keystore.key";

/// A key read from the store; `Debug` never shows it.
pub struct Secret(pub Zeroizing<String>);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Contents {
    v: u32,
    /// Every key the OS keychain held for xflow has been moved here, and every
    /// moved keychain copy is confirmed deleted.
    #[serde(default)]
    migrated_keychain: bool,
    /// Moved here, but the keychain copy isn't confirmed deleted yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pending_delete: BTreeSet<String>,
    /// Removed by the user while the migration is unfinished: never imported
    /// again, and their keychain copy is deleted (via `pending_delete`).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    removed: BTreeSet<String>,
    #[serde(default)]
    entries: BTreeMap<String, Sealed>,
}
#[derive(Serialize, Deserialize)]
struct Sealed {
    nonce: String,
    ct: String,
}
enum Parsed {
    Ok(Contents),
    Damaged,
    Newer,
}
/// One rule for reads and writes: a valid file of another version is never
/// touched; only an unparseable one may be replaced.
fn parse(bytes: &[u8]) -> Parsed {
    let version = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("v").and_then(serde_json::Value::as_u64));
    match version {
        Some(v) if v != 1 => Parsed::Newer,
        _ => serde_json::from_slice(bytes).map_or(Parsed::Damaged, Parsed::Ok),
    }
}

/// Identity of the credentials file; every write renames a new inode into place,
/// so a change here means another process (or this one) wrote the store.
pub type Stamp = (u64, u64, i64, i64, u64);

pub struct Store {
    config: PathBuf,
    data: PathBuf,
}
impl Store {
    /// Same layout as xflow-app's `paths`: config beside `config.toml`, key in data.
    #[cfg_attr(test, allow(unreachable_code))]
    pub fn open() -> Result<Self> {
        // Unit tests never touch the user's real store.
        #[cfg(test)]
        {
            let root = test_root();
            return Ok(Self::at(&root.join("config"), &root.join("data")));
        }
        let env = |name| std::env::var_os(name).filter(|v| !v.is_empty());
        let home = || env("HOME").map(PathBuf::from);
        let config = env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))
            .context("HOME or XDG_CONFIG_HOME is required")?;
        let data = env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local/share")))
            .context("HOME or XDG_DATA_HOME is required")?;
        Ok(Self::at(&config.join("xflow"), &data.join("xflow")))
    }
    pub fn at(config_dir: &Path, data_dir: &Path) -> Self {
        Self {
            config: config_dir.to_owned(),
            data: data_dir.to_owned(),
        }
    }
    pub fn stamp(&self) -> Option<Stamp> {
        let m = std::fs::symlink_metadata(self.config.join(CREDENTIALS)).ok()?;
        Some((m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.len()))
    }

    /// The stored key, and whether the keychain migration is complete.
    pub fn get(&self, provider: &str) -> Result<(Option<Secret>, bool)> {
        let contents = self.load()?;
        let Some(sealed) = contents.entries.get(provider) else {
            return Ok((None, contents.migrated_keychain));
        };
        let reenter = || format!("re-enter your key: `xflow key set {provider}`");
        let Some(cipher) = self.cipher(false)? else {
            bail!(
                "{} is missing; {}",
                self.data.join(KEY).display(),
                reenter()
            );
        };
        let nonce = STANDARD.decode(&sealed.nonce).ok();
        let mut data = Zeroizing::new(STANDARD.decode(&sealed.ct).unwrap_or_default());
        let plain = nonce
            .and_then(|n| Nonce::try_assume_unique_for_key(&n).ok())
            .and_then(|nonce| {
                cipher
                    .open_in_place(nonce, Aad::from(provider.as_bytes()), &mut data)
                    .ok()
            })
            .and_then(|plain| std::str::from_utf8(plain).ok())
            .ok_or_else(|| {
                anyhow!(
                    "stored {provider} key can't be decrypted (damaged, or keystore.key changed); {}",
                    reenter()
                )
            })?;
        let secret = Secret(Zeroizing::new(plain.to_owned()));
        Ok((Some(secret), contents.migrated_keychain))
    }
    pub fn set(&self, provider: &str, secret: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.load_for_write()?;
        let sealed = self.seal(provider, secret)?;
        contents.entries.insert(provider.to_owned(), sealed);
        contents.removed.remove(provider);
        self.save(&contents)
    }
    /// Removes the key for good: while the keychain migration is unfinished, the
    /// removal is recorded so no later (or in-flight) migration brings it back,
    /// and the keychain copy is queued for deletion.
    pub fn remove(&self, provider: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.load_for_write()?;
        let mut changed = contents.entries.remove(provider).is_some();
        if !contents.migrated_keychain {
            changed |= contents.removed.insert(provider.to_owned());
            changed |= contents.pending_delete.insert(provider.to_owned());
        }
        if changed {
            self.save(&contents)?;
        }
        Ok(())
    }
    /// The keychain copy of `provider` is confirmed deleted.
    pub fn deleted_from_keychain(&self, provider: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.load_for_write()?;
        if contents.pending_delete.remove(provider) {
            self.save(&contents)?;
        }
        Ok(())
    }
    pub fn migrated(&self) -> Result<bool> {
        Ok(self.load()?.migrated_keychain)
    }
    /// Refuses a store that is damaged or not private; the error names the fix.
    /// Ok(false) when nothing is stored yet.
    pub fn check(&self) -> Result<bool> {
        let exists = read_private(&self.config, CREDENTIALS)?.is_some();
        self.load()?;
        read_private(&self.data, KEY)?;
        Ok(exists)
    }

    /// Move keys out of the OS keychain, once. `read` is Ok(None) only when there
    /// is no item; Err (needs approval, service unavailable) leaves the item and
    /// the migration runs again next time. A keychain copy is deleted only after
    /// its stored copy reads back from disk (or the user removed the key); until
    /// `delete` confirms, it stays pending and is retried. A removed key is never
    /// imported. Returns the ids that couldn't be read.
    pub fn migrate(
        &self,
        ids: &[&str],
        read: impl Fn(&str) -> Result<Option<Zeroizing<String>>>,
        delete: impl Fn(&str) -> Result<()>,
    ) -> Result<Vec<String>> {
        let before = self.load()?;
        if before.migrated_keychain {
            return Ok(vec![]);
        }
        let (mut found, mut blocked) = (vec![], vec![]);
        // A key saved here wins: its keychain copy is neither read nor deleted
        // (unless this migration moved it and its deletion is pending). A key the
        // user removed is never read back.
        let skip = |id: &str| before.entries.contains_key(id) || before.removed.contains(id);
        for &id in ids.iter().filter(|id| !skip(id)) {
            match read(id) {
                Ok(Some(secret)) if !secret.is_empty() => found.push((id, secret)),
                Ok(_) => {}
                Err(_) => blocked.push(id.to_owned()),
            }
        }
        let (pending, removed) = if found.is_empty() {
            (before.pending_delete, before.removed)
        } else {
            // Read without the lock (a foreground read may wait on the user), then
            // merge into the current file: a concurrent `xflow key set` or
            // `xflow key remove` wins.
            let _lock = self.lock()?;
            let mut contents = self.load_for_write()?;
            for (id, secret) in &found {
                if contents.removed.contains(*id) {
                    continue;
                }
                if !contents.entries.contains_key(*id) {
                    let sealed = self.seal(id, secret)?;
                    contents.entries.insert((*id).to_owned(), sealed);
                }
                contents.pending_delete.insert((*id).to_owned());
            }
            self.save(&contents)?;
            (contents.pending_delete, contents.removed)
        };
        let deleted: Vec<_> = pending
            .iter()
            .filter(|id| removed.contains(*id) || matches!(self.get(id), Ok((Some(_), _))))
            .filter(|id| delete(id).is_ok())
            .collect();
        let done = blocked.is_empty() && deleted.len() == pending.len();
        if !deleted.is_empty() || done {
            let _lock = self.lock()?;
            let mut contents = self.load_for_write()?;
            for id in deleted {
                contents.pending_delete.remove(id);
            }
            contents.migrated_keychain = blocked.is_empty() && contents.pending_delete.is_empty();
            if contents.migrated_keychain {
                // The keychain is never read again: nothing left to keep out.
                contents.removed.clear();
            }
            self.save(&contents)?;
        }
        Ok(blocked)
    }

    fn seal(&self, provider: &str, secret: &str) -> Result<Sealed> {
        let cipher = self.cipher(true)?.context("keystore key unavailable")?;
        let mut nonce = [0; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow!("no secure randomness"))?;
        let mut data = Zeroizing::new(secret.as_bytes().to_vec());
        cipher
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(provider.as_bytes()),
                &mut *data,
            )
            .map_err(|_| anyhow!("could not encrypt API key"))?;
        Ok(Sealed {
            nonce: STANDARD.encode(nonce),
            ct: STANDARD.encode(&*data),
        })
    }
    /// The data key; with `create`, a missing or damaged one is replaced (callers
    /// hold the lock). Entries sealed under an old key then ask to be re-entered.
    fn cipher(&self, create: bool) -> Result<Option<LessSafeKey>> {
        let bytes = match read_private(&self.data, KEY)? {
            Some(bytes) if bytes.len() == 32 => bytes,
            Some(_) if !create => bail!(
                "{} is damaged; re-enter your keys with `xflow key set <provider>`",
                self.data.join(KEY).display()
            ),
            _ if !create => return Ok(None),
            _ => {
                let mut fresh = Zeroizing::new(vec![0; 32]);
                SystemRandom::new()
                    .fill(&mut fresh)
                    .map_err(|_| anyhow!("no secure randomness"))?;
                write_private(&self.data, KEY, &fresh)?;
                fresh
            }
        };
        let key = UnboundKey::new(&CHACHA20_POLY1305, &bytes)
            .map_err(|_| anyhow!("invalid keystore key"))?;
        Ok(Some(LessSafeKey::new(key)))
    }
    fn load(&self) -> Result<Contents> {
        let path = self.config.join(CREDENTIALS);
        match read_private(&self.config, CREDENTIALS)?.map(|bytes| parse(&bytes)) {
            None => Ok(Contents::default()),
            Some(Parsed::Ok(contents)) => Ok(contents),
            Some(Parsed::Damaged) => bail!(
                "{} is damaged; re-enter your keys with `xflow key set <provider>`",
                path.display()
            ),
            Some(Parsed::Newer) => newer(&path),
        }
    }
    /// Like `load`, but a damaged file is replaced: the user is re-entering keys.
    /// A newer version and unsafe permissions are still refused.
    fn load_for_write(&self) -> Result<Contents> {
        let fresh = Contents {
            v: 1,
            ..Contents::default()
        };
        match read_private(&self.config, CREDENTIALS)?.map(|bytes| parse(&bytes)) {
            Some(Parsed::Ok(contents)) => Ok(contents),
            Some(Parsed::Newer) => newer(&self.config.join(CREDENTIALS)),
            None | Some(Parsed::Damaged) => Ok(fresh),
        }
    }
    fn save(&self, contents: &Contents) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(contents)?;
        write_private(&self.config, CREDENTIALS, &bytes)
    }
    /// Serializes writers (CLI, TUI, xflowd); readers tolerate the atomic rename.
    fn lock(&self) -> Result<File> {
        let dir = Dir::open(&self.config, true)?.context("keystore directory unavailable")?;
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK;
        let file = dir.open_file("credentials.lock", flags, 0o600)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            bail!("could not lock {}", self.config.display());
        }
        Ok(file)
    }
}
fn newer<T>(path: &Path) -> Result<T> {
    bail!(
        "{} was written by a newer xflow; update xflow (nothing was changed)",
        path.display()
    )
}

#[cfg(test)]
pub(crate) fn test_root() -> PathBuf {
    // A child test process is pointed at its parent's store.
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        std::env::var_os("XFLOW_TEST_STORE").map_or_else(
            || std::env::temp_dir().join(format!("xflow-providers-test-{}", std::process::id())),
            PathBuf::from,
        )
    })
    .clone()
}

/// A private directory, opened once without following a symlink and then used
/// by descriptor, so a path swapped afterwards can't redirect reads or writes.
struct Dir {
    file: File,
    path: PathBuf,
}
impl Dir {
    /// None when absent and not `create`d. Created directories are 0700 from the
    /// start (no umask window); a created or written one is narrowed to 0700.
    fn open(path: &Path, create: bool) -> Result<Option<Dir>> {
        if create {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .with_context(|| format!("could not create {}", path.display()))?;
        }
        let name = CString::new(path.as_os_str().as_bytes())?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe { libc::open(name.as_ptr(), flags) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            bail!(
                "{} must be a directory, not a symlink; fix: make it a real directory owned by you",
                path.display()
            );
        }
        let dir = Dir {
            file: unsafe { File::from_raw_fd(fd) },
            path: path.to_owned(),
        };
        let metadata = dir.file.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} belongs to another user", path.display());
        }
        if create && metadata.mode() & 0o077 != 0 {
            dir.file
                .set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Some(dir))
    }
    /// Refuses a directory others can read or write.
    fn private(&self) -> Result<()> {
        if self.file.metadata()?.mode() & 0o077 != 0 {
            bail!(
                "{} is accessible to other users; fix: chmod 700 {}",
                self.path.display(),
                self.path.display()
            );
        }
        Ok(())
    }
    fn open_file(&self, name: &str, flags: i32, mode: libc::c_uint) -> std::io::Result<File> {
        let name = CString::new(name)?;
        let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags, mode) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
/// None when absent; refuses files, or their directory, that others can reach or
/// that aren't yours.
fn read_private(dir: &Path, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let path = dir.join(name);
    let Some(dir) = Dir::open(dir, false)? else {
        return Ok(None);
    };
    // Non-blocking: a FIFO planted here must fail the type check below, not hang
    // the open. Regular files ignore the flag.
    let file = match dir.open_file(name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!(
            "{} can't be opened; it must be a regular file, not a symlink",
            path.display()
        ),
    };
    // Checked only once a file exists: an older, broader config dir is fine
    // until xflow writes a key into it (and narrows it).
    dir.private()?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "{} belongs to another user; fix: delete it, then `xflow key set <provider>`",
            path.display()
        );
    }
    if metadata.mode() & 0o077 != 0 {
        bail!(
            "{} is readable by other users; fix: chmod 600 {}",
            path.display(),
            path.display()
        );
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        bail!("{} is too large", path.display());
    }
    Ok(Some(bytes))
}
/// Atomic: a fresh, exclusively created temp file is written and fsynced, then
/// renamed over `name`. A failed write removes it and leaves the old file intact.
fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = dir.join(name);
    let dir = Dir::open(dir, true)?.context("keystore directory unavailable")?;
    let mut random = [0u8; 8];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow!("no secure randomness"))?;
    let tmp = format!(".{name}.{:016x}.tmp", u64::from_le_bytes(random));
    let mut file = dir
        .open_file(&tmp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)
        .with_context(|| format!("could not write {}", path.display()))?;
    let (tmp_c, name_c) = (CString::new(tmp)?, CString::new(name)?);
    let fd = dir.file.as_raw_fd();
    let mut write = || -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        if unsafe { libc::renameat(fd, tmp_c.as_ptr(), fd, name_c.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        dir.file.sync_all()
    };
    write().map_err(|e| {
        unsafe { libc::unlinkat(fd, tmp_c.as_ptr(), 0) };
        anyhow!(e).context(format!("could not write {}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    const SENTINEL: &str = "gsk_SENTINEL_9f8e7d6c5b4a";
    fn store() -> (tempfile_dir::Dir, Store) {
        let dir = tempfile_dir::Dir::new();
        let store = Store::at(&dir.0.join("config"), &dir.0.join("data"));
        (dir, store)
    }
    fn get(store: &Store, id: &str) -> Result<Option<String>> {
        Ok(store.get(id)?.0.map(|s| s.0.to_string()))
    }
    fn file(store: &Store) -> PathBuf {
        store.config.join(CREDENTIALS)
    }
    fn key(store: &Store) -> PathBuf {
        store.data.join(KEY)
    }
    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    fn file_json(store: &Store) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(file(store)).unwrap()).unwrap()
    }
    fn write_json(store: &Store, value: &serde_json::Value) {
        std::fs::write(file(store), serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn round_trip_is_private_and_never_plaintext() {
        let (_dir, store) = store();
        assert_eq!(get(&store, "groq").unwrap(), None);
        store.set("groq", SENTINEL).unwrap();
        store.set("openai", "sk-other").unwrap();
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        assert_eq!(get(&store, "openai").unwrap().as_deref(), Some("sk-other"));
        assert_eq!(mode(&file(&store)), 0o600);
        assert_eq!(mode(&key(&store)), 0o600);
        assert_eq!(mode(file(&store).parent().unwrap()), 0o700);
        assert_eq!(mode(key(&store).parent().unwrap()), 0o700);
        for path in [&file(&store), &key(&store)] {
            let bytes = std::fs::read(path).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("SENTINEL"));
        }
        store.remove("groq").unwrap();
        assert_eq!(get(&store, "groq").unwrap(), None);
        assert!(get(&store, "openai").unwrap().is_some());
    }
    #[test]
    fn debug_and_errors_never_show_key_bytes() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        let secret = store.get("groq").unwrap().0.unwrap();
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        // A damaged entry fails without leaking any of it.
        let mut json = file_json(&store);
        let ct = json["entries"]["groq"]["ct"].as_str().unwrap().to_owned();
        let flipped = if ct.starts_with('A') { 'B' } else { 'A' };
        json["entries"]["groq"]["ct"] = format!("{flipped}{}", &ct[1..]).into();
        write_json(&store, &json);
        let error = store.get("groq").unwrap_err();
        let text = format!("{error:?} {error:#}");
        assert!(!text.contains("SENTINEL") && !text.contains(&ct[4..12]));
    }
    #[test]
    fn swapped_entries_fail_by_aad() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        store.set("openai", "sk-other").unwrap();
        let mut json = file_json(&store);
        let groq = json["entries"]["groq"].clone();
        json["entries"]["openai"] = groq;
        write_json(&store, &json);
        let error = get(&store, "openai").unwrap_err().to_string();
        assert!(error.contains("xflow key set openai"), "{error}");
        assert!(get(&store, "groq").is_ok());
    }
    #[test]
    fn tampered_or_truncated_file_asks_to_reenter() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        let bytes = std::fs::read(file(&store)).unwrap();
        std::fs::write(file(&store), &bytes[..bytes.len() / 2]).unwrap();
        assert!(get(&store, "groq")
            .unwrap_err()
            .to_string()
            .contains("xflow key set"));
        // Re-entering replaces the damaged file.
        store.set("groq", SENTINEL).unwrap();
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        for bad in ["", "AAAA", "not base64!"] {
            let mut json = file_json(&store);
            json["entries"]["groq"]["nonce"] = bad.into();
            write_json(&store, &json);
            assert!(get(&store, "groq").is_err());
        }
    }
    #[test]
    fn wrong_or_missing_key_file_asks_to_reenter() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        std::fs::write(key(&store), [7u8; 32]).unwrap();
        let error = get(&store, "groq").unwrap_err().to_string();
        assert!(error.contains("xflow key set groq"), "{error}");
        std::fs::remove_file(key(&store)).unwrap();
        let error = get(&store, "groq").unwrap_err().to_string();
        assert!(error.contains("missing") && error.contains("xflow key set groq"));
        // Setting again makes a new key; the entry decrypts under it.
        store.set("groq", SENTINEL).unwrap();
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
    }
    #[test]
    fn broad_permissions_are_refused_with_a_fix() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        for path in [&file(&store), &key(&store)] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let error = store.check().unwrap_err().to_string();
            assert!(error.contains("chmod 600"), "{error}");
            assert!(store.get("groq").is_err());
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        store.check().unwrap();
        // Writes refuse too, rather than silently fixing an exposed file.
        std::fs::set_permissions(file(&store), std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(store.set("groq", "new").is_err());
    }
    #[test]
    fn symlinked_credentials_are_refused() {
        let (dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        let elsewhere = dir.0.join("elsewhere");
        std::fs::rename(file(&store), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, file(&store)).unwrap();
        assert!(store.get("groq").is_err());
    }
    #[test]
    fn interrupted_write_keeps_the_old_file() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        // The temp file can't be created: the write fails before any rename.
        chmod(&store.config, 0o500);
        assert!(store.set("groq", "replacement").is_err());
        chmod(&store.config, 0o700);
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        let leftovers = std::fs::read_dir(&store.config)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }
    #[test]
    fn planted_temp_files_and_hardlinks_are_never_written_through() {
        let (dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        let unrelated = dir.0.join("unrelated");
        std::fs::write(&unrelated, "do not touch").unwrap();
        chmod(&unrelated, 0o600);
        for planted in ["keystore.tmp", ".keystore.key.tmp", "credentials.tmp"] {
            let dir = if planted.contains("credentials") {
                &store.config
            } else {
                &store.data
            };
            std::fs::hard_link(&unrelated, dir.join(planted)).unwrap();
        }
        std::fs::remove_file(key(&store)).unwrap();
        store.set("groq", SENTINEL).unwrap();
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "do not touch");
        for path in [key(&store), file(&store)] {
            assert_eq!(std::fs::metadata(path).unwrap().nlink(), 1);
        }
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
    }
    #[test]
    fn new_directories_are_private_even_with_an_open_umask() {
        let (dir, _) = store();
        let store = Store::at(&dir.0.join("a/b/config"), &dir.0.join("c/data"));
        let old = unsafe { libc::umask(0) };
        let result = store.set("groq", SENTINEL);
        unsafe { libc::umask(old) };
        result.unwrap();
        for path in [&store.config, &store.data] {
            assert_eq!(mode(path), 0o700);
        }
        assert_eq!(mode(&dir.0.join("a")), 0o700);
        assert_eq!(mode(&file(&store)), 0o600);
        assert_eq!(mode(&key(&store)), 0o600);
    }
    #[test]
    fn symlinked_or_shared_directories_are_refused() {
        let (dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        // A config dir others can enter is refused with the fix.
        chmod(&store.config, 0o777);
        let error = get(&store, "groq").unwrap_err().to_string();
        assert!(error.contains("chmod 700"), "{error}");
        assert!(store.check().is_err());
        chmod(&store.config, 0o700);
        // A config dir replaced by a symlink to a real store is refused too.
        let moved = dir.0.join("moved");
        std::fs::rename(&store.config, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &store.config).unwrap();
        let error = get(&store, "groq").unwrap_err().to_string();
        assert!(error.contains("not a symlink"), "{error}");
        assert!(store.set("groq", "x").is_err());
        // A broad config dir with no credentials yet (older installs) is fine.
        std::fs::remove_file(&store.config).unwrap();
        std::fs::create_dir(&store.config).unwrap();
        chmod(&store.config, 0o755);
        assert_eq!(get(&store, "groq").unwrap(), None);
        store.set("groq", SENTINEL).unwrap();
        assert_eq!(mode(&store.config), 0o700);
    }
    #[test]
    fn a_newer_store_is_never_overwritten() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        store.set("openai", "sk-other").unwrap();
        let mut json = file_json(&store);
        json["v"] = 2.into();
        write_json(&store, &json);
        let before = std::fs::read(file(&store)).unwrap();
        for error in [
            store.get("groq").unwrap_err(),
            store.set("groq", "x").unwrap_err(),
            store.remove("openai").unwrap_err(),
            store
                .migrate(&["groq"], |_| Ok(None), |_| Ok(()))
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("newer xflow"), "{error}");
        }
        assert_eq!(std::fs::read(file(&store)).unwrap(), before);
    }
    #[test]
    fn concurrent_writers_both_land() {
        let (_dir, store) = store();
        store.set("seed", "x").unwrap();
        let store = std::sync::Arc::new(store);
        let ids = ["groq", "openai", "deepgram", "mistral", "gemini", "cleanup"];
        let threads: Vec<_> = ids
            .iter()
            .map(|&id| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        store.set(id, &format!("{id}-{i}")).unwrap();
                    }
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        for id in ids {
            assert_eq!(get(&store, id).unwrap(), Some(format!("{id}-9")));
        }
    }
    #[test]
    fn migration_moves_once_and_leaves_unreadable_items() {
        let (_dir, store) = store();
        let keychain = RefCell::new(BTreeMap::from([
            ("groq", Some(SENTINEL)),
            ("openai", None), // exists, but reading it would prompt
        ]));
        let reads = RefCell::new(0);
        let read = |id: &str| -> Result<Option<Zeroizing<String>>> {
            *reads.borrow_mut() += 1;
            match keychain.borrow().get(id) {
                Some(Some(v)) => Ok(Some(Zeroizing::new(v.to_string()))),
                Some(None) => bail!("needs approval"),
                None => Ok(None),
            }
        };
        let delete = |id: &str| -> Result<()> {
            keychain.borrow_mut().remove(id);
            Ok(())
        };
        let ids = ["groq", "openai", "deepgram"];
        assert_eq!(store.migrate(&ids, read, delete).unwrap(), ["openai"]);
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        assert!(!keychain.borrow().contains_key("groq"));
        assert!(keychain.borrow().contains_key("openai"));
        assert!(!store.migrated().unwrap());
        // The blocked item gets approved later: moved, and the flag is set.
        keychain.borrow_mut().insert("openai", Some("sk-later"));
        assert!(store.migrate(&ids, read, delete).unwrap().is_empty());
        assert_eq!(get(&store, "openai").unwrap().as_deref(), Some("sk-later"));
        assert!(store.migrated().unwrap());
        // Never again, even if the keychain gains an item.
        let before = *reads.borrow();
        keychain.borrow_mut().insert("deepgram", Some("dg"));
        assert!(store.migrate(&ids, read, delete).unwrap().is_empty());
        assert_eq!(*reads.borrow(), before);
        assert_eq!(get(&store, "deepgram").unwrap(), None);
    }
    #[test]
    fn migration_never_overwrites_a_key_set_here() {
        let (_dir, store) = store();
        store.set("groq", "new-key").unwrap();
        let deleted = RefCell::new(vec![]);
        let blocked = store
            .migrate(
                &["groq"],
                |_| panic!("a key set here is never read from the keychain"),
                |id| {
                    deleted.borrow_mut().push(id.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        assert!(blocked.is_empty());
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some("new-key"));
        assert!(deleted.borrow().is_empty());
    }

    #[test]
    fn failed_or_interrupted_delete_is_retried_and_completes_later() {
        let (_dir, store) = store();
        let keychain = RefCell::new(BTreeMap::from([("groq", SENTINEL)]));
        let read = |id: &str| -> Result<Option<Zeroizing<String>>> {
            Ok(keychain
                .borrow()
                .get(id)
                .map(|v| Zeroizing::new(v.to_string())))
        };
        // The delete fails (or the process dies before it): the copy is stored,
        // the keychain item stays, and nothing is marked done.
        let blocked = store
            .migrate(&["groq"], read, |_| bail!("delete failed"))
            .unwrap();
        assert!(blocked.is_empty());
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        assert!(!store.migrated().unwrap());
        assert!(file_json(&store)["pending_delete"][0] == "groq");
        // Next run ("restart"): the pending delete is retried and confirmed.
        let deleted = RefCell::new(vec![]);
        let delete = |id: &str| -> Result<()> {
            deleted.borrow_mut().push(id.to_owned());
            keychain.borrow_mut().remove(id);
            Ok(())
        };
        store.migrate(&["groq"], read, delete).unwrap();
        assert_eq!(*deleted.borrow(), ["groq"]);
        assert!(keychain.borrow().is_empty());
        assert!(store.migrated().unwrap());
        assert!(file_json(&store).get("pending_delete").is_none());
    }
    #[test]
    fn keychain_copy_is_kept_when_the_stored_copy_doesnt_read_back() {
        let (_dir, store) = store();
        let read = |_: &str| Ok(Some(Zeroizing::new(SENTINEL.to_owned())));
        store
            .migrate(&["groq"], read, |_| bail!("delete failed"))
            .unwrap();
        // The stored copy becomes unreadable before the retry: never delete then.
        std::fs::write(key(&store), [9u8; 32]).unwrap();
        let blocked = store
            .migrate(&["groq"], read, |_| panic!("deleted an unverified copy"))
            .unwrap();
        assert!(blocked.is_empty());
        assert!(!store.migrated().unwrap());
    }
    #[test]
    fn service_outage_leaves_migration_pending_until_it_recovers() {
        let (_dir, store) = store();
        let up = std::cell::Cell::new(false);
        let read = |_: &str| -> Result<Option<Zeroizing<String>>> {
            if !up.get() {
                bail!("Secret Service unavailable");
            }
            Ok(Some(Zeroizing::new(SENTINEL.to_owned())))
        };
        assert_eq!(
            store.migrate(&["groq"], read, |_| Ok(())).unwrap(),
            ["groq"]
        );
        assert!(!store.migrated().unwrap());
        up.set(true);
        assert!(store
            .migrate(&["groq"], read, |_| Ok(()))
            .unwrap()
            .is_empty());
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        assert!(store.migrated().unwrap());
    }
    #[test]
    fn removed_key_is_never_restored_by_migration() {
        let (_dir, store) = store();
        let keychain = RefCell::new(BTreeMap::from([("groq", SENTINEL)]));
        let read = |id: &str| -> Result<Option<Zeroizing<String>>> {
            Ok(keychain
                .borrow()
                .get(id)
                .map(|v| Zeroizing::new(v.to_string())))
        };
        let ids = ["groq", "openai"];
        // Moved, but the keychain delete fails; then the user removes the key and
        // the remove's own keychain delete fails too.
        store
            .migrate(&ids, read, |_| bail!("delete failed"))
            .unwrap();
        store.remove("groq").unwrap();
        for _ in 0..2 {
            store
                .migrate(&ids, read, |_| bail!("delete failed"))
                .unwrap();
            assert_eq!(get(&store, "groq").unwrap(), None, "removed key came back");
            assert!(!store.migrated().unwrap());
        }
        // The deletion goes through later: done, and still gone.
        let delete = |id: &str| -> Result<()> {
            keychain.borrow_mut().remove(id);
            Ok(())
        };
        store.migrate(&ids, read, delete).unwrap();
        assert!(keychain.borrow().is_empty());
        assert!(store.migrated().unwrap());
        assert_eq!(get(&store, "groq").unwrap(), None);
        let json = file_json(&store);
        assert!(json.get("removed").is_none() && json.get("pending_delete").is_none());
    }
    #[test]
    fn removal_during_an_in_flight_migration_wins() {
        let (_dir, store) = store();
        // The read runs outside the lock; another process removes the key before
        // the migration commits its (now stale) read.
        let read = |_: &str| -> Result<Option<Zeroizing<String>>> {
            store.remove("groq").unwrap();
            Ok(Some(Zeroizing::new(SENTINEL.to_owned())))
        };
        let deleted = RefCell::new(vec![]);
        let delete = |id: &str| -> Result<()> {
            deleted.borrow_mut().push(id.to_owned());
            Ok(())
        };
        store.migrate(&["groq"], read, delete).unwrap();
        assert_eq!(get(&store, "groq").unwrap(), None);
        assert_eq!(*deleted.borrow(), ["groq"]);
        assert!(store.migrated().unwrap());
    }
    #[test]
    fn a_key_set_again_after_removal_is_kept() {
        let (_dir, store) = store();
        store.remove("groq").unwrap();
        store.set("groq", "fresh").unwrap();
        let read = |_: &str| Ok(Some(Zeroizing::new("old".to_owned())));
        store.migrate(&["groq"], read, |_| Ok(())).unwrap();
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some("fresh"));
        assert!(store.migrated().unwrap());
    }
    #[test]
    fn a_fifo_in_place_of_a_store_file_is_refused_not_waited_on() {
        for name in [CREDENTIALS, KEY] {
            let (_dir, store) = store();
            store.set("groq", SENTINEL).unwrap();
            let dir = if name == KEY {
                &store.data
            } else {
                &store.config
            };
            std::fs::remove_file(dir.join(name)).unwrap();
            let path = CString::new(dir.join(name).as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            let (tx, rx) = std::sync::mpsc::channel();
            let (config, data) = (store.config.clone(), store.data.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Store::at(&config, &data).get("groq").map(|_| ()));
            });
            let result = rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or_else(|_| panic!("reading a FIFO {name} hung"));
            let error = result.unwrap_err().to_string();
            assert!(error.contains("not a regular file"), "{error}");
        }
    }
    #[test]
    fn check_reports_whether_anything_is_stored() {
        let (_dir, store) = store();
        assert!(!store.check().unwrap());
        store.set("groq", SENTINEL).unwrap();
        assert!(store.check().unwrap());
    }

    /// A self-deleting temp dir; no new dev-dependency for one helper.
    mod tempfile_dir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "xflow-keystore-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
