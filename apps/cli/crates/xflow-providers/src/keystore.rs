//! xflow's own credential store. Provider keys are sealed with ChaCha20-Poly1305
//! (the provider id is the AAD, so entries can't be swapped) in
//! `$XDG_CONFIG_HOME/xflow/credentials`, under 32 random bytes kept apart in
//! `$XDG_DATA_HOME/xflow/keystore.key`. Both are 0600 in 0700 directories.
//!
//! Threat model: encrypted at rest against casual disclosure (`cat`, backups, a
//! copied or committed config dir, other OS users). Not protection from malware
//! running as you, which can read both files.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAX_FILE: u64 = 1 << 20;

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
    /// Every key the OS keychain held for xflow has been moved here.
    #[serde(default)]
    migrated_keychain: bool,
    #[serde(default)]
    entries: BTreeMap<String, Sealed>,
}
#[derive(Serialize, Deserialize)]
struct Sealed {
    nonce: String,
    ct: String,
}

pub struct Store {
    file: PathBuf,
    key: PathBuf,
}
impl Store {
    /// Same layout as xflow-app's `paths`: config beside `config.toml`, key in data.
    #[cfg_attr(test, allow(unreachable_code))]
    pub fn open() -> Result<Self> {
        // Unit tests never touch the user's real store.
        #[cfg(test)]
        {
            static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
            let root = ROOT.get_or_init(|| {
                std::env::temp_dir().join(format!("xflow-providers-test-{}", std::process::id()))
            });
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
            file: config_dir.join("credentials"),
            key: data_dir.join("keystore.key"),
        }
    }

    /// The stored key, and whether the keychain migration is complete.
    pub fn get(&self, provider: &str) -> Result<(Option<Secret>, bool)> {
        let contents = self.load()?;
        let Some(sealed) = contents.entries.get(provider) else {
            return Ok((None, contents.migrated_keychain));
        };
        let reenter = || format!("re-enter your key: `xflow key set {provider}`");
        let Some(cipher) = self.cipher(false)? else {
            bail!("{} is missing; {}", self.key.display(), reenter());
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
        self.save(&contents)
    }
    pub fn remove(&self, provider: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.load_for_write()?;
        if contents.entries.remove(provider).is_some() {
            self.save(&contents)?;
        }
        Ok(())
    }
    pub fn migrated(&self) -> Result<bool> {
        Ok(self.load()?.migrated_keychain)
    }
    /// Refuses files that are damaged or not private; the error names the fix.
    pub fn check(&self) -> Result<()> {
        self.load()?;
        read_private(&self.key)?;
        Ok(())
    }

    /// Move keys out of the OS keychain, once. `read` is Ok(None) for no item and
    /// Err for an item it couldn't read without asking; those stay put, and the
    /// migration runs again next time. Items are deleted only after the store has
    /// their copy on disk. Returns the ids left in the keychain.
    pub fn migrate(
        &self,
        ids: &[&str],
        read: impl Fn(&str) -> Result<Option<Zeroizing<String>>>,
        delete: impl Fn(&str),
    ) -> Result<Vec<String>> {
        let before = self.load()?;
        if before.migrated_keychain {
            return Ok(vec![]);
        }
        let (mut found, mut blocked) = (vec![], vec![]);
        // A key saved here wins: its keychain copy is neither read nor deleted.
        for &id in ids.iter().filter(|id| !before.entries.contains_key(**id)) {
            match read(id) {
                Ok(Some(secret)) if !secret.is_empty() => found.push((id, secret)),
                Ok(_) => {}
                Err(_) => blocked.push(id.to_owned()),
            }
        }
        {
            // Read without the lock (a foreground read may wait on the user), then
            // merge into the current file: a concurrent `xflow key set` wins.
            let _lock = self.lock()?;
            let mut contents = self.load_for_write()?;
            for (id, secret) in &found {
                if !contents.entries.contains_key(*id) {
                    let sealed = self.seal(id, secret)?;
                    contents.entries.insert((*id).to_owned(), sealed);
                }
            }
            contents.migrated_keychain = blocked.is_empty();
            self.save(&contents)?;
        }
        for (id, _) in found {
            delete(id);
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
        let bytes = match read_private(&self.key)? {
            Some(bytes) if bytes.len() == 32 => bytes,
            Some(_) if !create => bail!(
                "{} is damaged; re-enter your keys with `xflow key set <provider>`",
                self.key.display()
            ),
            _ if !create => return Ok(None),
            _ => {
                let mut fresh = Zeroizing::new(vec![0; 32]);
                SystemRandom::new()
                    .fill(&mut fresh)
                    .map_err(|_| anyhow!("no secure randomness"))?;
                write_private(&self.key, &fresh)?;
                fresh
            }
        };
        let key = UnboundKey::new(&CHACHA20_POLY1305, &bytes)
            .map_err(|_| anyhow!("invalid keystore key"))?;
        Ok(Some(LessSafeKey::new(key)))
    }
    fn load(&self) -> Result<Contents> {
        let Some(bytes) = read_private(&self.file)? else {
            return Ok(Contents::default());
        };
        let contents: Contents = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow!(
                "{} is damaged; re-enter your keys with `xflow key set <provider>`",
                self.file.display()
            )
        })?;
        if contents.v != 1 {
            bail!("{} was written by a newer xflow", self.file.display());
        }
        Ok(contents)
    }
    /// Like `load`, but a damaged file is replaced: the user is re-entering keys.
    /// Unsafe permissions are still refused.
    fn load_for_write(&self) -> Result<Contents> {
        let contents = match read_private(&self.file)? {
            Some(bytes) => serde_json::from_slice::<Contents>(&bytes)
                .ok()
                .filter(|c| c.v == 1),
            None => None,
        };
        Ok(contents.unwrap_or(Contents {
            v: 1,
            ..Contents::default()
        }))
    }
    fn save(&self, contents: &Contents) -> Result<()> {
        write_private(&self.file, &serde_json::to_vec_pretty(contents)?)
    }
    /// Serializes writers (CLI, TUI, xflowd); readers tolerate the atomic rename.
    fn lock(&self) -> Result<File> {
        let dir = self.file.parent().context("invalid credentials path")?;
        private_dir(dir)?;
        let file = open(&dir.join("credentials.lock"), true)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                bail!("could not lock {}", dir.display());
            }
        }
        Ok(file)
    }
}

/// Open without following a symlink; `write` creates the file 0600.
fn open(path: &Path, write: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(!write).write(write).create(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}
/// None when absent; refuses files that others can read or that aren't yours.
fn read_private(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let file = match open(path, false) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!(
            "{} can't be opened; it must be a regular file, not a symlink",
            path.display()
        ),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
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
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        bail!("{} is too large", path.display());
    }
    Ok(Some(bytes))
}
/// Atomic: a temp file is written and fsynced, then renamed over `path`. A failed
/// write leaves the previous file intact.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("invalid keystore path")?;
    private_dir(dir)?;
    let tmp = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = open(&tmp, true)?;
        file.set_len(0)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    };
    write().with_context(|| format!("could not write {}", path.display()))
}
/// 0700 and owned by you; created if missing.
fn private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::metadata(dir)?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} is not your directory", dir.display());
        }
        if metadata.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
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
    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    fn file_json(store: &Store) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&store.file).unwrap()).unwrap()
    }
    fn write_json(store: &Store, value: &serde_json::Value) {
        std::fs::write(&store.file, serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn round_trip_is_private_and_never_plaintext() {
        let (_dir, store) = store();
        assert_eq!(get(&store, "groq").unwrap(), None);
        store.set("groq", SENTINEL).unwrap();
        store.set("openai", "sk-other").unwrap();
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
        assert_eq!(get(&store, "openai").unwrap().as_deref(), Some("sk-other"));
        assert_eq!(mode(&store.file), 0o600);
        assert_eq!(mode(&store.key), 0o600);
        assert_eq!(mode(store.file.parent().unwrap()), 0o700);
        assert_eq!(mode(store.key.parent().unwrap()), 0o700);
        for path in [&store.file, &store.key] {
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
        let bytes = std::fs::read(&store.file).unwrap();
        std::fs::write(&store.file, &bytes[..bytes.len() / 2]).unwrap();
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
        std::fs::write(&store.key, [7u8; 32]).unwrap();
        let error = get(&store, "groq").unwrap_err().to_string();
        assert!(error.contains("xflow key set groq"), "{error}");
        std::fs::remove_file(&store.key).unwrap();
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
        for path in [&store.file, &store.key] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let error = store.check().unwrap_err().to_string();
            assert!(error.contains("chmod 600"), "{error}");
            assert!(store.get("groq").is_err());
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        store.check().unwrap();
        // Writes refuse too, rather than silently fixing an exposed file.
        std::fs::set_permissions(&store.file, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(store.set("groq", "new").is_err());
    }
    #[test]
    fn symlinked_credentials_are_refused() {
        let (dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        let elsewhere = dir.0.join("elsewhere");
        std::fs::rename(&store.file, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &store.file).unwrap();
        assert!(store.get("groq").is_err());
    }
    #[test]
    fn interrupted_write_keeps_the_old_file() {
        let (_dir, store) = store();
        store.set("groq", SENTINEL).unwrap();
        // The temp file can't be created: the write fails before any rename.
        std::fs::create_dir(store.file.with_extension("tmp")).unwrap();
        assert!(store.set("groq", "replacement").is_err());
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some(SENTINEL));
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
        let delete = |id: &str| {
            keychain.borrow_mut().remove(id);
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
                |id| deleted.borrow_mut().push(id.to_owned()),
            )
            .unwrap();
        assert!(blocked.is_empty());
        assert_eq!(get(&store, "groq").unwrap().as_deref(), Some("new-key"));
        assert!(deleted.borrow().is_empty());
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
