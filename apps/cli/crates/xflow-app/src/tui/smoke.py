#!/usr/bin/env python3
"""Exercise the real TUI in a PTY against mock IPC, with fully isolated XDG roots.

Run from the worktree: python3 apps/cli/crates/xflow-app/src/tui/smoke.py
No audio, desktop delivery, keyring service, real daemon, or provider HTTP calls.
"""
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[6]
BINARY = ROOT / "apps/cli/target/debug/xflow"


def run(online, columns, rows):
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
            data = dict(ok=True, state=state[0], level=0.0, version="mock", protocol=3)
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
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
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
            # Exercise resize while live, including the tiny-terminal fallback.
            for width, height in ((34, 11), (columns, rows)):
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
                process.send_signal(signal.SIGWINCH)
                drain()
                assert process.poll() is None
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
                key(b"8")
                key(b"-")
                assert b"OpenRouter" in output, "chat setup list"
                key(b"\x1b")
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
            print(json.dumps(dict(online=online, size=f"{columns}x{rows}", idle_cpu_ticks=idle_ticks, requests=[r["command"] for r in requests], saved_config=True, terminal_restored=True, resize_safe=True)))
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


def chat(columns, rows):
    """The Chat page against a loopback streaming server: consent, answer, cancel, retry."""
    requests = []

    class Mock(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(body)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            def event(data):
                self.wfile.write(b"data: " + data + b"\n\n")
                self.wfile.flush()
            def text(value):
                event(json.dumps(dict(choices=[dict(delta=dict(content=value))])).encode())
            # The first "slow" question stalls until the client hangs up.
            if "slow" in json.dumps(body["messages"][-1]) and sum("slow" in json.dumps(r["messages"][-1]) for r in requests) == 1:
                text("partial-text")
                time.sleep(4)
                return
            text("finished-answer")
            event(json.dumps(dict(choices=[dict(delta={}, finish_reason="stop")])).encode())
            event(b"[DONE]")

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(dir=ROOT / "target/tui-tmp") as scratch:
        base = Path(scratch)
        for name in ("run", "cfg", "data"):
            (base / name).mkdir(mode=0o700)
        config = base / "cfg/xflow/config.toml"
        config.parent.mkdir(mode=0o700)
        config.write_text(f"[stt]\nprovider='custom'\nendpoint='http://127.0.0.1:1/transcribe'\nmodel='mock'\n[ai]\nprovider='custom'\nmodel='mock'\nendpoint='http://127.0.0.1:{server.server_port}/v1/chat/completions'\n")
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY") and not k.endswith("_API_KEY")}
        env.update(XDG_RUNTIME_DIR=str(base / "run"), XDG_CONFIG_HOME=str(base / "cfg"), XDG_DATA_HOME=str(base / "data"), DBUS_SESSION_BUS_ADDRESS=f"unix:path={base}/no-session-bus", TERM="xterm-256color", COLORTERM="truecolor")
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        before = termios.tcgetattr(slave)
        process = subprocess.Popen([BINARY, "tui"], stdin=slave, stdout=slave, stderr=slave, env=env, cwd=base)
        output = bytearray()

        def drain(duration=0.3):
            deadline = time.monotonic() + duration
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.03)[0]:
                    output.extend(os.read(master, 65536))

        def key(value, duration=0.3):
            os.write(master, value)
            drain(duration)

        def shown(text):
            found = text in output
            assert found, f"{text} not in {bytes(output[-2000:])}"

        try:
            drain(0.8)
            key(b"-")
            # Command letters are typing, and the first send asks.
            key("quit? 日本語 private-question\r".encode())
            shown(b"Allow it?")
            assert process.poll() is None, "typing q quit"
            key(b"y", 0.6)
            shown(b"finished-answer")
            assert "consented" in config.read_text() and "private-question" not in config.read_text()
            output.clear()
            key(b"slow\r", 0.5)
            shown(b"partial-text")
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 14, 44, 0, 0))
            process.send_signal(signal.SIGWINCH)
            drain()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
            process.send_signal(signal.SIGWINCH)
            key(b"\x1b", 0.5)
            shown(b"Interrupted")
            output.clear()
            key(b"\x12", 0.6)  # Ctrl+R resends.
            shown(b"finished-answer")
            stat = Path(f"/proc/{process.pid}/stat")
            ticks = lambda: sum(map(int, stat.read_text().split()[13:15]))
            start = ticks()
            drain(1)
            idle_ticks = ticks() - start
            assert idle_ticks <= 1, f"idle CPU ticks={idle_ticks}"
            paste = "नमस्ते 日本語 ".encode() * 3500
            started = time.monotonic()
            key(b"\x1b[200~" + paste + b"\x1b[201~", 0.3)
            key(b"!", 0.1)
            paste_ms = int((time.monotonic() - started - 0.4) * 1000)
            assert process.poll() is None
            key(b"\x15")  # Ctrl+U.
            key(b"\x03", 0.2)  # Ctrl+C.
            process.wait(timeout=3)
            drain()
            assert process.returncode == 0, bytes(output[-2000:])
            assert termios.tcgetattr(slave) == before, "terminal attributes not restored"
            leaked = [p for p in base.rglob("*") if p.is_file() and b"private-question" in p.read_bytes()]
            assert not leaked, f"chat text on disk: {leaked}"
            print(json.dumps(dict(chat=True, size=f"{columns}x{rows}", model_requests=len(requests), idle_cpu_ticks=idle_ticks, paste_bytes=len(paste), paste_overhead_ms=paste_ms, on_disk=False, terminal_restored=True)))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            server.shutdown()
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    chat(80, 24)
    for columns, rows in ((80, 24), (120, 35)):
        run(False, columns, rows)
        run(True, columns, rows)
