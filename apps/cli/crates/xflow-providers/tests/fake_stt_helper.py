"""Fake xflow-stt-coreml for supervisor tests (tools/local-stt/PROTOCOL.md).

Usage: fake_stt_helper.py MODE DIR. Logs events to DIR/log; never transcribes.
"""
import json
import os
import struct
import sys
import time

mode, root = sys.argv[1], sys.argv[2]
stdin, stdout = sys.stdin.buffer, sys.stdout.buffer


def log(line):
    with open(os.path.join(root, "log"), "a") as f:
        f.write(line + "\n")


def read_exact(n):
    data = b""
    while len(data) < n:
        chunk = stdin.read(n - len(data))
        if not chunk:
            sys.exit(0)
        data += chunk
    return data


def read_frame():
    (n,) = struct.unpack("<I", read_exact(4))
    header = json.loads(read_exact(n))
    (m,) = struct.unpack("<I", read_exact(4))
    return header, read_exact(m)


def write_raw(header_bytes, payload=b""):
    stdout.write(struct.pack("<I", len(header_bytes)) + header_bytes)
    stdout.write(struct.pack("<I", len(payload)) + payload)
    stdout.flush()


def write(header):
    write_raw(json.dumps(header).encode())


def once(name):
    """True the first time `name` is asked for across helper restarts."""
    flag = os.path.join(root, name)
    if os.path.exists(flag):
        return False
    open(flag, "w").close()
    return True


log("pid %d" % os.getpid())
log("env " + " ".join(sorted(os.environ)))
while True:
    header, payload = read_frame()
    op = header["op"]
    if op == "hello":
        log("hello " + header["model_dir"])
        if mode == "bad_json":
            write_raw(b"{not json")
            sys.exit(2)
        if mode == "oversize":
            stdout.write(struct.pack("<I", 70000))
            stdout.flush()
            time.sleep(5)
            sys.exit(2)
        if mode == "load_error":
            write({"op": "error", "code": "model_invalid", "message": "bad model"})
            sys.exit(1)
        if mode == "slow_load":
            time.sleep(0.5)
        write({"op": "progress", "stage": "loading"})
        write({"op": "ready", "protocol": 1, "helper": "fake", "load_ms": 1})
    elif op == "transcribe":
        log("transcribe")
        if mode == "hang":
            time.sleep(60)
        if mode == "crash_first" and once("crashed"):
            sys.exit(1)
        if mode == "short_payload_first" and once("short"):
            header_bytes = json.dumps({"op": "result", "id": header["id"], "text": "x"}).encode()
            stdout.write(struct.pack("<I", len(header_bytes)) + header_bytes)
            stdout.write(struct.pack("<I", 10) + b"abc")
            stdout.flush()
            sys.exit(1)
        if mode == "bad_audio_first" and once("bad"):
            write({"op": "error", "id": header["id"], "code": "bad_audio", "message": "bad"})
            continue
        (first,) = struct.unpack("<h", payload[:2])
        text = "bytes=%d first=%d" % (len(payload), first)
        write({"op": "result", "id": header["id"], "text": text, "audio_ms": 1, "compute_ms": 1})
    elif op == "shutdown":
        log("shutdown")
        sys.exit(0)
