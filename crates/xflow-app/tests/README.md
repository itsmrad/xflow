# Daemon end-to-end tests

`daemon_e2e.rs` starts the real `xflowd` binary with an empty environment, fresh
XDG directories, a disabled desktop bus, and a loopback OpenAI-compatible mock.
It covers the socket actor, multipart PCM WAV upload, state subscriptions,
delivery and SQLite history, cancellation, provider errors and recovery, silence,
offline endpoint rejection, concurrent clients and malformed frames.

Run through the shared Cargo gate on the development host:

```sh
PATH=/home/mrad/.cache/xflow-dev/bin:$PATH sh scripts/check.sh
```

`scripts/check.sh` enables `xflow-app/test-support`, so CI and local verification
run the same suite. To run only this harness:

```sh
PATH=/home/mrad/.cache/xflow-dev/bin:$PATH cargo test -p xflow-app --locked --features test-support --test daemon_e2e
```

If the shared `/tmp` quota is exhausted, point `TMPDIR` to an owned scratch
directory on the worktree filesystem. Every fixture and child is removed on exit,
including assertion failures; only children created by the test are killed.

The seam is compiled only with the explicit `test-support` feature. In that build,
`XFLOW_TEST_AUDIO=voice|silence` and `XFLOW_TEST_SINK` must be set together;
`XFLOW_TEST_ROOT` must be an owned, private, canonical directory. The XDG runtime,
config and data directories must be its `run`, `config` and `data` children, the
sink must be `delivered.txt` inside it, and display variables must be absent.
`voice` yields one second of deterministic 16 kHz mono PCM; `silence` fails before
upload. File delivery uses a private file and refuses symlinks. The production
daemon change is only adapter selection in `daemon::serve`; release packaging
builds without the feature and cannot activate this seam.

The reload scenario is temporarily ignored until the daemon worker's Reload
milestone is integrated. It checks switching endpoints and retaining the previous
config after an invalid edit. Remove that ignore when the milestone is available.

These tests do not validate real microphone drivers, GNOME rendering, clipboard,
paste/type delivery, external provider availability, paid calls, or native ARM
release execution. The separate D-Bus bridge test runs on a private session bus;
extension checks validate JavaScript syntax, Node logic tests and GSettings
schemas. Tag release publishing requires a real release run and is not exercised
by ordinary CI.
