#!/usr/bin/env python3
"""Exercise the real TUI in a PTY against mock IPC, with fully isolated XDG roots.

Run from the worktree: python3 crates/xflow-app/src/tui/smoke.py
No audio, desktop delivery, keyring service, real daemon, or provider HTTP calls.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[4]
BINARY = ROOT / "target/debug/xflow"


def run(online):
    with tempfile.TemporaryDirectory(dir=ROOT / "target/tui-tmp") as scratch:
        base = Path(scratch)
        for name in ("run", "cfg", "data"):
            (base / name).mkdir(mode=0o700)
        config = base / "cfg/xflow/config.toml"
        if online:
            config.parent.mkdir(mode=0o700)
            config.write_text("# isolated smoke config\n[stt]\nprovider='custom'\nendpoint='http://127.0.0.1:1/transcribe'\nmodel='mock'\n")
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY") and not k.endswith("_API_KEY")}
        env.update(XDG_RUNTIME_DIR=str(base / "run"), XDG_CONFIG_HOME=str(base / "cfg"), XDG_DATA_HOME=str(base / "data"), DBUS_SESSION_BUS_ADDRESS=f"unix:path={base}/no-session-bus", TERM="xterm-256color", COLORTERM="truecolor")
        subscribers = []
        requests = []
        state = ["idle"]
        stop = threading.Event()
        listener = None
        threads = []
        clients = []
        entry = dict(id=42, created_at=1790859600, text="Mock transcript with timing", provider="mock", duration_ms=2000, latency_ms=27, mode="dictation")

        def reply(client, **extra):
            data = dict(ok=True, state=state[0], level=0.0, version="mock", protocol=2)
            data.update(extra)
            client.sendall(json.dumps(data).encode() + b"\n")

        def handle(client):
            try:
                data = b""
                while not data.endswith(b"\n"):
                    part = client.recv(4096)
                    if not part:
                        return
                    data += part
                request = json.loads(data)
                requests.append(request)
                command = request["command"]
                if command == "subscribe":
                    subscribers.append(client)
                    reply(client)
                    stop.wait(20)
                elif command == "toggle":
                    state[0] = "listening" if state[0] == "idle" else "success"
                    for subscriber in subscribers:
                        reply(subscriber, text=entry["text"] if state[0] == "success" else None, timings=dict(audio_ms=2000, stt_ms=20, total_ms=27))
                    reply(client)
                elif command == "history":
                    reply(client, history=[entry] if not request.get("query") or request["query"] in entry["text"] else [], total=1)
                elif command == "history_get":
                    reply(client, entry=entry)
                elif command == "stats":
                    reply(client, stats=dict(sessions=1, words=4, audio_ms=2000, sessions_today=1, words_today=4, streak_days=1, wpm=120.0, latency_p50_ms=27, latency_p95_ms=27))
                else:
                    reply(client)
            except (OSError, ValueError):
                pass
            finally:
                client.close()

        def serve():
            while not stop.is_set():
                try:
                    client, _ = listener.accept()
                except socket.timeout:
                    continue
                except OSError:
                    break
                clients.append(client)
                thread = threading.Thread(target=handle, args=(client,))
                threads.append(thread)
                thread.start()

        if online:
            runtime = base / "run/xflow"
            runtime.mkdir(mode=0o700)
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(runtime / "daemon.sock"))
            listener.listen()
            listener.settimeout(0.2)
            acceptor = threading.Thread(target=serve)
            acceptor.start()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        before = termios.tcgetattr(slave)
        process = subprocess.Popen([BINARY, "tui"], stdin=slave, stdout=slave, stderr=slave, env=env, cwd=base)
        output = bytearray()

        def drain(duration=0.2):
            deadline = time.monotonic() + duration
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], min(0.03, max(0, deadline - time.monotonic())))
                if ready:
                    try:
                        output.extend(os.read(master, 65536))
                    except OSError:
                        break

        def key(value):
            os.write(master, value)
            drain()

        try:
            drain(0.8)
            assert process.poll() is None, output.decode(errors="replace")
            if online:
                stat = Path(f"/proc/{process.pid}/stat")
                ticks = lambda: sum(map(int, stat.read_text().split()[13:15]))
                start = ticks()
                drain(1)
                idle_ticks = ticks() - start
                assert idle_ticks <= 1, f"idle CPU ticks={idle_ticks}"
                key(b" ")
                key(b" ")
                key(b"2")
                key(b"/Mock\r")
                key(b"9")
                assert any(r["command"] == "stats" for r in requests)
                assert any(r["command"] == "history" and r.get("query") == "Mock" for r in requests)
            else:
                # Differential painting skips unchanged spaces using cursor moves.
                assert b"Getting" in output and b"started" in output
                idle_ticks = None
            key(b"3Nsmoke-word\r")
            key(b"4Nsignature\tThanks, Ada\x1bOQ")  # F2 submits a multi-field form.
            key(b"\x13")  # Ctrl+S: writes isolated config and sends mock Reload.
            drain(0.4)
            assert config.exists()
            assert "smoke-word" in config.read_text()
            assert "Thanks, Ada" in config.read_text()
            assert "isolated smoke config" in config.read_text() if online else True
            key(b"t\x1b[C\r")
            key(b"\x13")
            assert 'theme = "midnight"' in config.read_text()
            key(b"q")
            process.wait(timeout=3)
            drain()
            assert process.returncode == 0, output.decode(errors="replace")
            assert termios.tcgetattr(slave) == before, "terminal attributes not restored"
            assert b"\x1b[?1049l" in output, "alternate screen not restored"
            print(json.dumps(dict(online=online, idle_cpu_ticks=idle_ticks, requests=[r["command"] for r in requests], saved_config=True, terminal_restored=True)))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            stop.set()
            if listener:
                listener.close()
                acceptor.join(timeout=2)
            for client in clients:
                client.close()
            for thread in threads:
                thread.join(timeout=2)
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    run(False)
    run(True)
