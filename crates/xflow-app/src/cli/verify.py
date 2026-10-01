#!/usr/bin/env python3
"""Isolated CLI acceptance tests: fake daemon/tools; no microphone/keyring/cloud.

Run after cargo build -p xflow-app --bin xflow:
python3 crates/xflow-app/src/cli/verify.py target/debug/xflow
"""
import csv
import io
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time


class Daemon:
    def __init__(self, root):
        self.requests = []
        self.subscribers = []
        self.protocol = 2
        self.fail = False
        self.closed = threading.Event()
        self.lock = threading.Lock()
        self.socket = socket.socket(socket.AF_UNIX)
        directory = root / "run/xflow"
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.socket.bind(str(directory / "daemon.sock"))
        self.socket.listen()
        self.socket.settimeout(0.1)
        self.thread = threading.Thread(target=self.run)
        self.thread.start()

    def reply(self, connection, **fields):
        payload = {"ok": True, "state": "idle", "level": 0, **fields}
        connection.sendall(json.dumps(payload).encode() + b"\n")

    def run(self):
        while not self.closed.is_set():
            try:
                connection, _ = self.socket.accept()
            except socket.timeout:
                continue
            connection.settimeout(2)
            with connection.makefile("rb") as reader:
                request = json.loads(reader.readline())
            with self.lock:
                self.requests.append(request)
            command = request["command"]
            if command == "subscribe":
                # A stale last transcript in the snapshot must not become listen's result.
                self.reply(connection, protocol=self.protocol, version="0.1.0", text="stale")
                self.subscribers.append(connection)
                continue
            if self.fail:
                self.reply(connection, ok=False, message="mock failure")
            elif command == "status":
                self.reply(connection, protocol=self.protocol, version="0.1.0", provider="local", model="test")
            elif command == "history":
                entries = [{"id": 7, "created_at": 42, "provider": "local", "text": 'hello, "you"\nagain'}]
                self.reply(connection, history=entries if request.get("offset", 0) == 0 else [], total=1)
            elif command == "history_get":
                self.reply(connection, entry={"id": 7, "created_at": 42, "provider": "local", "text": "saved"})
            elif command == "last":
                self.reply(connection, text="saved")
            else:
                self.reply(connection, state="processing" if command == "stop" else "idle")
                if command == "stop":
                    for subscriber in self.subscribers:
                        try:
                            self.reply(subscriber, state="success", text="fresh dictation")
                        except (BrokenPipeError, ConnectionResetError):
                            pass
            connection.close()

    def close(self):
        self.closed.set()
        self.thread.join(timeout=3)
        self.socket.close()
        for connection in self.subscribers:
            connection.close()
        assert not self.thread.is_alive()


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="xflow-cli-", dir=os.environ.get("TMPDIR")) as directory:
        root = Path(directory)
        for name in ("cfg", "data", "run", "bin"):
            (root / name).mkdir(mode=0o700)
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "VISUAL", "EDITOR")}
        env.update(XDG_CONFIG_HOME=str(root / "cfg"), XDG_DATA_HOME=str(root / "data"), XDG_RUNTIME_DIR=str(root / "run"),
                   XDG_CURRENT_DESKTOP="", XDG_SESSION_TYPE="", GSETTINGS_BACKEND="memory", NO_COLOR="1", TMPDIR=str(root),
                   PATH=str(root / "bin") + os.pathsep + env["PATH"], XFLOW_TOOL_LOG=str(root / "tools.log"))

        def run(*args, code=0, stdin=None):
            result = subprocess.run([str(binary), *args], env=env, input=stdin, text=True, capture_output=True, timeout=15)
            assert result.returncode == code, (args, result.returncode, result.stdout, result.stderr)
            return result

        assert "setup" in run("--help").stdout
        assert "complete" in run("completions", "bash").stdout
        assert "script" in json.loads(run("completions", "zsh", "--json").stdout)
        assert run("listen", "--seconds", "0", code=2).stderr
        run("setup", "--yes", "--provider", "groq")
        assert json.loads(run("setup", "--yes", "--json").stdout)["provider"] == "groq"
        config = root / "cfg/xflow/config.toml"
        assert config.stat().st_mode & 0o777 == 0o600
        run("config", "set", "recording.max_seconds", "30")
        before = config.read_bytes()
        run("config", "set", "recording.max_seconds", "0", code=4)
        assert config.read_bytes() == before
        assert json.loads(run("config", "show", "--json").stdout)["recording"]["max_seconds"] == 30
        assert json.loads(run("config", "get", "recording.max_seconds", "--json").stdout)["value"] == 30
        run("config", "reset", code=2)
        run("dictionary", "add", "XFlow", "Postgres")
        run("dictionary", "replace", "post grass", "Postgres")
        words = json.loads(run("dictionary", "--json").stdout)
        assert words["words"] == ["XFlow", "Postgres"] and words["replacements"][0]["to"] == "Postgres"
        run("dictionary", "unreplace", "post grass")
        run("snippets", "add", "my email", "me@example.com")
        assert json.loads(run("snippets", "--json").stdout)[0]["trigger"] == "my email"
        run("styles", "add", "chat", "--apps", "slack,discord", "--mode", "light")
        assert json.loads(run("styles", "--json").stdout)[0]["apps"] == ["slack", "discord"]
        editor = root / "bin/mock-editor"
        editor.write_text('#!/bin/sh\nprintf "[stt]\\nunknown = true\\n" > "$1"\n')
        editor.chmod(0o755)
        env["EDITOR"] = str(editor)
        before = config.read_bytes()
        run("config", "edit", code=4)
        assert config.read_bytes() == before
        run("config", "reset", "--yes")
        assert json.loads(run("config", "show", "--json").stdout)["dictionary"]["words"] == []
        assert json.loads(run("doctor", "--json", code=6).stdout)["ok"] is False
        run("status", code=3)
        run("key", "set", "groq", "--stdin", stdin="", code=2)

        for tool in ("systemctl", "gnome-extensions", "glib-compile-schemas", "journalctl"):
            path = root / "bin" / tool
            path.write_text('#!/bin/sh\nprintf "%s " "$0" "$@" >> "$XFLOW_TOOL_LOG"\nprintf "\\n" >> "$XFLOW_TOOL_LOG"\nprintf "mock tool output\\n"\n')
            path.chmod(0o755)
        run("service", "install")
        assert "graphical-session.target" in (root / "cfg/systemd/user/xflow.service").read_text()
        run("daemon", "start")
        run("daemon", "logs", "--json")
        run("extension", "install")
        assets = root / "data/gnome-shell/extensions/xflow@xflow.local"
        assert (assets / "extension.js").is_file()
        run("extension", "enable")
        run("extension", "uninstall")
        assert not (assets / "extension.js").exists()
        run("service", "uninstall")
        assert not (root / "cfg/systemd/user/xflow.service").exists()
        source = root / "prebuilt"
        source.mkdir()
        for name in ("xflow", "xflowd"):
            path = source / name
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        prefix = root / "prefix with spaces"
        installer = Path(__file__).resolve().parents[4] / "scripts/install.sh"
        installed = subprocess.run(["sh", str(installer), "--prefix", str(prefix), "--bin-dir", str(source)], env=env, capture_output=True, text=True, timeout=10)
        assert installed.returncode == 0, installed.stderr
        assert all((prefix / "bin" / name).stat().st_mode & 0o777 == 0o755 for name in ("xflow", "xflowd"))

        daemon = Daemon(root)
        try:
            assert json.loads(run("status", "--json").stdout)["protocol"] == 2
            assert run("status").stdout == "FIELD     VALUE\nState     idle\nProvider  local\nModel     test\nDaemon    0.1.0\nProtocol  2\n"
            assert json.loads(run("doctor", "--json").stdout)["ok"] is True
            run("toggle", "--quiet")
            run("start", "--command", "--json")
            run("stop")
            run("cancel")
            run("copy-last")
            run("paste-last")
            assert run("last").stdout == "saved\n"
            assert json.loads(run("history", "--json").stdout)["total"] == 1
            assert run("history", "show", "7").stdout == "saved\n"
            csv_text = run("history", "export", "--format", "csv").stdout
            assert list(csv.reader(io.StringIO(csv_text)))[1][-1] == 'hello, "you"\nagain'
            run("history", "search", "hello")
            run("history", "delete", "7")
            run("history", "clear", "--yes")
            run("config", "set", "sounds.enabled", "false")
            assert any(r["command"] == "reload" for r in daemon.requests)
            reloads = len([r for r in daemon.requests if r["command"] == "reload"])
            run("--config", str(root / "alternate.toml"), "config", "set", "sounds.enabled", "false")
            assert len([r for r in daemon.requests if r["command"] == "reload"]) == reloads
            result = run("listen", "--once", "--seconds", "1", "--quiet")
            assert result.stdout == "fresh dictation\n", result.stdout
            starts = [r for r in daemon.requests if r["command"] == "start" and r.get("delivery") == "none"]
            assert starts
            listen = subprocess.Popen([str(binary), "listen", "--once", "--seconds", "10"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and len([r for r in daemon.requests if r.get("delivery") == "none"]) < 2:
                    time.sleep(0.01)
                listen.send_signal(signal.SIGINT)
                _, stderr = listen.communicate(timeout=5)
                assert listen.returncode == 130, stderr
                assert daemon.requests[-1]["command"] == "cancel"
            finally:
                if listen.poll() is None:
                    listen.kill()
                    listen.wait()
            daemon.protocol = 1
            assert "Restart" in run("status", code=3).stderr
            daemon.protocol = 2
            daemon.fail = True
            failure = run("toggle", "--json", code=1)
            assert not failure.stdout and json.loads(failure.stderr)["error"] == "mock failure"
        finally:
            daemon.close()
        print("CLI acceptance passed: config/personalization, fake system tools, IPC/history/listen/cancel, errors and completions")


if __name__ == "__main__":
    main()
