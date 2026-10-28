# Provider keys

`xflow key set <provider>` (or the TUI's API key screen) stores the key in xflow's own keystore. It never goes to `config.toml` or the OS keychain, and storing or reading it never shows an OS prompt.

| File | Contents | Mode |
|---|---|---|
| `$XDG_CONFIG_HOME/xflow/credentials` (default `~/.config/xflow/`) | versioned JSON; one ChaCha20-Poly1305 ciphertext and a random nonce per provider, with the provider id as associated data | 0600, dir 0700 |
| `$XDG_DATA_HOME/xflow/keystore.key` (default `~/.local/share/xflow/`) | 32 random bytes, the encryption key | 0600, dir 0700 |

Writes are atomic (a fresh, exclusively created temp file, fsync, rename, all relative to the already-validated directory) and serialized across `xflow`, the TUI and `xflowd` with a `credentials.lock` next to the file. An environment variable (`GROQ_API_KEY`, or `stt.api_key_env`) still overrides the stored key. `xflow key status` and `xflow doctor` show which source is used.

## Threat model

The keystore is **encrypted at rest against casual disclosure of your config dir**: `cat`, screen sharing, a `~/.config` copied, synced or committed to a dotfiles repo (the key lives in the data dir, not the config dir), and other OS users. It is **not protection from malware running as you**, which can read both files, just as it can read `gh`, `aws`, `npm` or `docker` tokens. It is also **not protection for full backups**: a home-folder or Time Machine backup holds both `credentials` and `keystore.key`, so anyone who can read that backup can decrypt the keys on another machine. Protect the backup (FileVault, encrypted Time Machine) as you would the Mac. The key is deliberately not bound to the machine, so a migrated Mac keeps working. A Keychain item with a correct access list is stronger. xflow gives that up because, for a self-signed or rebuilt CLI, the Keychain re-prompts after every update, and a prompt in a background daemon breaks dictation.

## Fixes

xflow refuses a file that is a symlink, belongs to another user, or is readable by others, and a key directory that is a symlink or that others can enter (`chmod 700 <dir>`). An older, broader `~/.config/xflow` is narrowed to 0700 the first time a key is stored. A store written by a newer xflow is refused for both reading and writing, and is never overwritten. The error and `xflow doctor` give the fix (`chmod 600 <file>`). A damaged file, a swapped entry, or a missing or different `keystore.key` gives "re-enter your key: `xflow key set <provider>`". The secret is never shown in errors, logs or `Debug` output.

## Moving keys from the Keychain (older builds)

Older builds kept keys in the login Keychain (Secret Service on Linux). The move happens once:

- **xflowd** (and any key lookup) reads each item with user interaction disabled. On macOS 26 that read either succeeds or fails within ~25 ms with `errSecAuthFailed`, and never shows a dialog. A key that reads is written to the keystore and read back. Only then is the Keychain item deleted with `/usr/bin/security delete-generic-password`, which needs no approval. A deletion that fails, or never happens because the process exits, stays recorded as pending and is retried. Once every item is moved and confirmed deleted, xflow never queries the Keychain again.
- An item written by a *different build* (the usual case: every rebuild changes the code hash the item's access list trusts) can't be read silently. It stays where it is, and `xflow key status` reports `keychain (not migrated…)`. Run `xflow key status` in a terminal once: it prints "Moving your key out of the Keychain. macOS may ask once.", reads the item with the normal macOS prompt, moves it, and deletes the item. Or run `xflow key set <provider>` to skip the prompt entirely.
- On Linux, the Secret Service may ask to unlock, so only the foreground `xflow key status` reads it. Only "no such item" counts as absent: while the Secret Service is down or locked, the move stays pending and is retried.
- A running `xflowd` notices a key changed by any `xflow` process (any `--config`) on its next request, with no reload.
- `XFLOW_NO_KEYCHAIN=1` disables all keychain access (CI and test harnesses).
