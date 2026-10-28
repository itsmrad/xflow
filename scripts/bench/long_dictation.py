#!/usr/bin/env python3
"""Long-dictation check: feed synthetic speech through the real xflowd and Groq.

Builds macOS `say` speech WAVs (48 kHz mono, numbered "Checkpoint N." sentences),
streams each through a `test-support` xflowd as a live microphone
(XFLOW_TEST_AUDIO=wav, never speakers), and reports transcript coverage,
stop->text latency and daemon RSS. The key comes only from GROQ_API_KEY
(`set -a; . .tools/groq.env; set +a`); the Keychain is never touched.

    cd apps/cli && cargo build --release -p xflow-app --features test-support --bin xflowd
    python3 -B scripts/bench/long_dictation.py --minutes 3 6 12
"""
import argparse, difflib, json, os, re, shutil, socket, subprocess, tempfile, threading, time, wave
from pathlib import Path

POOL = [
    "the deploy pipeline compiles the Rust workspace and runs every integration test before release",
    "we moved the session cache from Redis into Postgres to keep the latency budget under fifty milliseconds",
    "the daemon drains the microphone buffer every second so memory stays flat during long dictations",
    "please review the pull request tomorrow morning and leave comments on the error handling",
    "the onboarding flow asks for microphone access first and then for accessibility permissions",
    "our billing service retries failed webhooks with exponential backoff and a dead letter queue",
    "the terminal user interface renders the history table with keyboard shortcuts for every action",
    "after the migration we will delete the old configuration keys and update the documentation",
    "the provider returns a transcript within a second for most clips under one minute long",
    "write a unit test for the chunker that checks every sample survives the split",
    "the website uses a monospace font with thin grid lines and a single accent colour",
    "remember to rotate the development API key once the release candidate ships",
]


def chunk_text(chunk):
    """Twelve sentences (~80 s of `say -r 175`)."""
    return " ".join(f"Checkpoint {chunk * 12 + i + 1}. {POOL[(chunk + i) % 12].capitalize()}." for i in range(12))


def speech(directory, minutes):
    """A WAV of whole chunks reaching `minutes`; returns (path, chunk count)."""
    durations, chunk = [], 0
    while sum(durations) < minutes * 60 - 5:
        wav = directory / f"chunk{chunk}.wav"
        if not wav.exists():
            aiff = directory / f"chunk{chunk}.aiff"
            subprocess.run(["say", "-r", "175", "-o", aiff, chunk_text(chunk)], check=True)
            subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@48000", "-c", "1", aiff, wav], check=True)
        with wave.open(str(wav)) as r:
            durations.append(r.getnframes() / r.getframerate())
        chunk += 1
    out = directory / f"speech{minutes}.wav"
    with wave.open(str(out), "wb") as w:
        for i in range(chunk):
            with wave.open(str(directory / f"chunk{i}.wav")) as r:
                if i == 0:
                    w.setparams(r.getparams())
                w.writeframes(r.readframes(r.getnframes()))
    return out, chunk


def run(daemon, wav, chunks, speed, cleanup):
    root = Path(os.path.realpath(tempfile.mkdtemp(prefix="xfl", dir="/private/tmp")))  # AF_UNIX path limit
    try:
        root.chmod(0o700)
        for d in ["run", "config/xflow", "data", "home"]:
            (root / d).mkdir(parents=True, mode=0o700, exist_ok=True)
        shutil.copy(wav, root / "input.wav")
        polish = 'mode = "light"\nprovider = "groq"\napi_key_env = "GROQ_API_KEY"\n' if cleanup else 'mode = "raw"\n'
        (root / "config/xflow/config.toml").write_text(
            f'[stt]\nprovider = "groq"\napi_key_env = "GROQ_API_KEY"\n[cleanup]\n{polish}'
            '[recording]\nmin_ms = 0\n[injection]\ntrailing_space = false\n'
            '[sounds]\nenabled = false\n[notifications]\nenabled = false\n')
        env = {"HOME": str(root / "home"), "GROQ_API_KEY": os.environ["GROQ_API_KEY"],
               "XDG_RUNTIME_DIR": str(root / "run"), "XDG_CONFIG_HOME": str(root / "config"),
               "XDG_DATA_HOME": str(root / "data"), "DBUS_SESSION_BUS_ADDRESS": f"unix:path={root}/no-bus",
               "XFLOW_TEST_ROOT": str(root), "XFLOW_TEST_AUDIO": "wav", "XFLOW_TEST_SINK": str(root / "delivered.txt"),
               "XFLOW_TEST_AUDIO_SPEED": str(speed)}
        with open(root / "daemon.log", "w") as log:
            child = subprocess.Popen([daemon], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
        sock = str(root / "run/xflow/daemon.sock")
        try:
            return measure(child, sock, root, chunks)
        finally:
            child.kill()
            child.wait()
    finally:
        shutil.rmtree(root)


def measure(child, sock, root, chunks):
    def rss():
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(child.pid)], capture_output=True, text=True).stdout
        return int(out or 0) / 1024

    def request(command):
        with socket.socket(socket.AF_UNIX) as s:
            s.connect(sock)
            s.sendall(json.dumps({"command": command}).encode() + b"\n")
            return json.loads(s.makefile().readline())

    for _ in range(500):
        if os.path.exists(sock):
            break
        time.sleep(0.02)
    time.sleep(0.5)
    idle, peak, done = rss(), [0.0], threading.Event()

    def sample():
        while not done.wait(1):
            peak[0] = max(peak[0], rss())

    threading.Thread(target=sample, daemon=True).start()
    with socket.socket(socket.AF_UNIX) as sub:
        sub.connect(sock)
        sub.sendall(b'{"command":"subscribe"}\n')
        events = sub.makefile()
        events.readline()
        assert request("start")["ok"]
        stopped = None  # the capture finishes at the end of the file
        while True:
            event = json.loads(events.readline())
            if event["state"] == "processing" and stopped is None:
                stopped = time.time()
            if stopped and event["state"] in ("success", "error", "idle"):
                latency = time.time() - stopped
                break
    done.set()
    text = (root / "delivered.txt").read_text() if (root / "delivered.txt").exists() else ""
    words = lambda s: re.findall(r"[a-z0-9]+", s.lower())
    reference = " ".join(chunk_text(c) for c in range(chunks))
    found = [int(n) for n in re.findall(r"checkpoint\s*(\d+)", text, re.I)]
    expected = range(1, chunks * 12 + 1)
    return {
        "state": event["state"], "message": event.get("message"), "timings": event.get("timings"),
        "stop_to_text_ms": round(latency * 1000),
        "checkpoints": f"{len(set(found) & set(expected))}/{len(expected)}",
        "missing": sorted(set(expected) - set(found)), "in_order": found == sorted(found),
        "words": f"{len(words(text))}/{len(words(reference))}",
        "word_similarity": round(difflib.SequenceMatcher(None, words(reference), words(text), autojunk=False).ratio(), 4),
        "rss_idle_mb": round(idle, 1), "rss_peak_mb": round(peak[0], 1),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--daemon", default="apps/cli/target/release/xflowd")
    parser.add_argument("--minutes", type=int, nargs="+", default=[3, 6, 12])
    parser.add_argument("--speed", type=float, default=1.0, help="times real time (Groq rate limits apply)")
    parser.add_argument("--cleanup", action="store_true", help="also run Groq light cleanup")
    parser.add_argument("--wav-dir", default="/private/tmp/xflow-long-dictation")
    args = parser.parse_args()
    if not os.environ.get("GROQ_API_KEY"):
        parser.error("GROQ_API_KEY is required (the Keychain is never read)")
    directory = Path(args.wav_dir)
    directory.mkdir(parents=True, exist_ok=True)
    for minutes in args.minutes:
        wav, chunks = speech(directory, minutes)
        result = run(os.path.abspath(args.daemon), wav, chunks, args.speed, args.cleanup)
        print(json.dumps({"wav": wav.name, "speed": args.speed, "cleanup": args.cleanup, **result}))


if __name__ == "__main__":
    main()
