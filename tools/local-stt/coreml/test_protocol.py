#!/usr/bin/env python3
"""Drive xflow-stt-coreml. `--self-test` checks this driver against `--fake-helper`."""

import json
import os
import select
import struct
import subprocess
import sys
import tempfile
import threading

MAX_HEADER = 65536
MAX_PAYLOAD = 41943040
PCM_RATE = 16000
SILENCE = b"\x00" * (PCM_RATE * 2)  # 1 second of 16-bit mono


class ProtocolError(Exception):
    pass


def pack(obj, payload=b""):
    header = json.dumps(obj, separators=(",", ":")).encode()
    return struct.pack("<I", len(header)) + header + struct.pack("<I", len(payload)) + payload


class Reader:
    def __init__(self, read):
        self._read = read
        self.buf = b""

    def exact(self, count):
        while len(self.buf) < count:
            chunk = self._read(count - len(self.buf))
            if not chunk:
                return None if not self.buf else self.buf
            self.buf += chunk
        out, self.buf = self.buf[:count], self.buf[count:]
        return out

    def frame(self):
        raw = self.exact(4)
        if raw is None:
            return None
        if len(raw) != 4:
            raise ProtocolError("truncated length")
        (hlen,) = struct.unpack("<I", raw)
        if hlen < 1 or hlen > MAX_HEADER:
            raise ProtocolError(f"header_len {hlen}")
        header = self.exact(hlen)
        if header is None or len(header) != hlen:
            raise ProtocolError("truncated header")
        raw = self.exact(4)
        if raw is None or len(raw) != 4:
            raise ProtocolError("truncated payload length")
        (plen,) = struct.unpack("<I", raw)
        if plen > MAX_PAYLOAD:
            raise ProtocolError(f"payload_len {plen}")
        if plen == 0:
            payload = b""
        else:
            payload = self.exact(plen)
            if payload is None or len(payload) != plen:
                raise ProtocolError("truncated payload")
        try:
            obj = json.loads(header.decode("utf-8"))
        except (UnicodeError, json.JSONDecodeError) as exc:
            raise ProtocolError("bad json") from exc
        if not isinstance(obj, dict) or not isinstance(obj.get("op"), str):
            raise ProtocolError("missing op")
        return obj, payload


def audio_ms(pcm):
    return (len(pcm) // 2) * 1000 // PCM_RATE


def read_wav_pcm(path):
    with open(path, "rb") as handle:
        data = handle.read()
    if len(data) < 12 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise ValueError(f"{path} is not a RIFF WAVE file")
    pos = 12
    fmt = None
    pcm = None
    while pos + 8 <= len(data):
        chunk = data[pos : pos + 4]
        size = struct.unpack_from("<I", data, pos + 4)[0]
        body = data[pos + 8 : pos + 8 + size]
        if len(body) != size:
            raise ValueError(f"{path} has a truncated {chunk!r} chunk")
        if chunk == b"fmt ":
            if len(body) < 16:
                raise ValueError(f"{path} has a short fmt chunk")
            fmt = struct.unpack_from("<HHIIHH", body)
        elif chunk == b"data":
            pcm = body
        pos += 8 + size + (size & 1)
    if fmt is None or pcm is None:
        raise ValueError(f"{path} is missing fmt or data")
    audio_format, channels, rate, _byte_rate, _block, bits = fmt
    if (audio_format, channels, rate, bits) != (1, 1, PCM_RATE, 16):
        raise ValueError(f"{path} must be 16 kHz mono 16-bit PCM")
    if len(pcm) == 0 or len(pcm) % 2:
        raise ValueError(f"{path} has an empty or odd-sized data chunk")
    return pcm


class Chunked:
    """Returns at most `size` bytes per read. Records how far the caller read."""

    def __init__(self, data, size=1):
        self.data = data
        self.size = size
        self.offset = 0
        self.max_request = 0

    def __call__(self, count):
        self.max_request = max(self.max_request, count)
        take = min(count, self.size, len(self.data) - self.offset)
        if take <= 0:
            return b""
        out = self.data[self.offset : self.offset + take]
        self.offset += take
        return out


def check(name, ok, detail=""):
    if ok:
        print(f"PASS {name}")
    else:
        print(f"FAIL {name}: {detail}")
    return ok


def framing_tests():
    ok = True
    payload = b"\x01\x00\xff\x7f"
    blob = pack({"op": "transcribe", "id": 7, "sample_rate": 16000}, payload)
    blob += pack({"op": "shutdown"})
    source = Chunked(blob, 1)
    reader = Reader(source)
    first = reader.frame()
    second = reader.frame()
    third = reader.frame()
    ok &= check(
        "framing partial reads",
        first == ({"op": "transcribe", "id": 7, "sample_rate": 16000}, payload)
        and second == ({"op": "shutdown"}, b"")
        and third is None
        and source.offset == len(blob),
        f"frames={first!r} {second!r} next={third!r} offset={source.offset}",
    )
    huge = struct.pack("<I", MAX_HEADER + 1) + b"unused"
    limited = Chunked(huge, 1)
    try:
        Reader(limited).frame()
        raised = ""
    except ProtocolError as exc:
        raised = str(exc)
    ok &= check(
        "framing rejects oversize header before the body",
        raised.startswith("header_len") and limited.offset == 4 and limited.max_request <= 4,
        f"error={raised!r} offset={limited.offset} max_request={limited.max_request}",
    )
    return ok


class Session:
    def __init__(self, command, timeout):
        self.timeout = timeout
        self.proc = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0
        )
        self.stderr = bytearray()
        self._err = threading.Thread(target=self._drain, daemon=True)
        self._err.start()
        self.reader = Reader(self._read)

    def _drain(self):
        while True:
            chunk = os.read(self.proc.stderr.fileno(), 65536)
            if not chunk:
                break
            self.stderr.extend(chunk)

    def _read(self, count):
        readable, _, _ = select.select([self.proc.stdout.fileno()], [], [], self.timeout)
        if not readable:
            raise TimeoutError("timed out waiting for the helper")
        return os.read(self.proc.stdout.fileno(), count)

    def send(self, obj, payload=b""):
        self.proc.stdin.write(pack(obj, payload))
        self.proc.stdin.flush()

    def send_raw(self, data):
        self.proc.stdin.write(data)
        self.proc.stdin.flush()

    def response(self):
        while True:
            item = self.reader.frame()
            if item is None:
                raise ProtocolError("helper closed stdout")
            obj, _payload = item
            if obj.get("op") == "progress":
                continue
            return obj

    def exit_code(self, close_stdin=True, timeout=None):
        if close_stdin:
            self.proc.stdin.close()
        code = self.proc.wait(timeout=self.timeout if timeout is None else timeout)
        self._err.join(timeout=5)
        return code

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=5)
        self._err.join(timeout=5)

    def err_text(self):
        return self.stderr.decode("utf-8", "replace")[-400:]


def is_int(value):
    return type(value) is int


def run_suite(command, model_dir, wavs):
    failures = 0

    def case(name, ok, detail=""):
        nonlocal failures
        if not check(name, ok, detail):
            failures += 1

    clips = []
    for path in wavs:
        try:
            clips.append((path, read_wav_pcm(path)))
        except ValueError as exc:
            case(f"transcribe {os.path.basename(path)}", False, str(exc))
    if not clips:
        case("wav files", False, "pass at least one 16 kHz mono 16-bit wav")
        return 1

    version = subprocess.run(command + ["--version"], capture_output=True, timeout=30)
    try:
        parsed = json.loads(version.stdout.decode())
        version_ok = (
            version.returncode == 0
            and parsed.get("protocol") == 1
            and isinstance(parsed.get("helper"), str)
            and parsed["helper"] != ""
        )
    except json.JSONDecodeError:
        version_ok = False
    case("--version", version_ok, f"status={version.returncode} stdout={version.stdout!r}")

    def hello_session():
        local = Session(command, 300)
        try:
            local.send({"op": "hello", "protocol": 1, "model_dir": model_dir})
            reply = local.response()
        except Exception:
            local.kill()
            raise
        if reply.get("op") != "ready" or reply.get("protocol") != 1 or not isinstance(reply.get("helper"), str):
            detail = f"hello reply {reply!r} stderr={local.err_text()!r}"
            local.kill()
            raise ProtocolError(detail)
        if not is_int(reply.get("load_ms")) or reply["load_ms"] < 0:
            local.kill()
            raise ProtocolError(f"load_ms {reply.get('load_ms')!r}")
        return local

    def transcribe(session, pcm, request_id):
        session.send({"op": "transcribe", "id": request_id, "sample_rate": PCM_RATE}, pcm)
        reply = session.response()
        if reply.get("op") != "result" or reply.get("id") != request_id:
            raise ProtocolError(f"result {reply!r} stderr={session.err_text()!r}")
        if not isinstance(reply.get("text"), str) or not is_int(reply.get("compute_ms")):
            raise ProtocolError(f"result fields {reply!r}")
        if reply.get("audio_ms") != audio_ms(pcm) or reply["compute_ms"] < 0:
            raise ProtocolError(f"timing {reply!r}")
        print(f"text: {reply['text']}")
        print(f"compute_ms: {reply['compute_ms']}")
        return reply

    session = None
    try:
        session = hello_session()
        for index, (path, pcm) in enumerate(clips, start=1):
            try:
                transcribe(session, pcm, index)
                case(f"transcribe {os.path.basename(path)}", True)
            except (ProtocolError, TimeoutError, OSError) as exc:
                case(f"transcribe {os.path.basename(path)}", False, str(exc))
                break
        code = session.exit_code()
        case("EOF after transcribe exits 0", code == 0, f"status={code} stderr={session.err_text()!r}")
    except (ProtocolError, TimeoutError, OSError) as exc:
        case("hello", False, str(exc))
        if session:
            session.kill()
    session = None

    joined = SILENCE.join(pcm for _path, pcm in clips)
    try:
        session = hello_session()
        transcribe(session, joined, 1)
        code = session.exit_code()
        case("long audio", code == 0, f"status={code}")
    except (ProtocolError, TimeoutError, OSError) as exc:
        case("long audio", False, str(exc))
        if session:
            session.kill()
    session = None

    def owned(name, send, code_name, status, close_stdin=True):
        local = Session(command, 60)
        try:
            send(local)
            reply = local.response()
            got = local.exit_code(close_stdin=close_stdin)
            case(
                name,
                reply.get("op") == "error" and reply.get("code") == code_name and got == status,
                f"reply={reply!r} status={got} stderr={local.err_text()!r}",
            )
        except (ProtocolError, TimeoutError, OSError, subprocess.TimeoutExpired) as exc:
            case(name, False, str(exc))
            local.kill()

    owned(
        "wrong protocol version",
        lambda s: s.send({"op": "hello", "protocol": 2, "model_dir": model_dir}),
        "protocol",
        1,
    )
    owned(
        "missing model dir",
        lambda s: s.send({"op": "hello", "protocol": 1, "model_dir": "/no/such/xflow-stt-model"}),
        "model_missing",
        1,
    )
    owned(
        "header over 64 KiB",
        lambda s: s.send_raw(struct.pack("<I", MAX_HEADER + 1)),
        "protocol",
        2,
    )
    owned(
        "bad JSON",
        lambda s: s.send_raw(pack_raw(b"{")),
        "protocol",
        2,
    )
    owned(
        "unknown op",
        lambda s: s.send({"op": "nope"}),
        "protocol",
        2,
    )

    try:
        session = hello_session()
        session.send({"op": "transcribe", "id": 11, "sample_rate": 44100}, b"\x00\x00")
        rate = session.response()
        case(
            "sample_rate 44100",
            rate.get("op") == "error" and rate.get("code") == "bad_audio" and rate.get("id") == 11,
            repr(rate),
        )
        session.send({"op": "transcribe", "id": 12, "sample_rate": PCM_RATE}, b"\x00\x00\x00")
        odd = session.response()
        case(
            "odd payload length",
            odd.get("op") == "error" and odd.get("code") == "bad_audio" and odd.get("id") == 12,
            repr(odd),
        )
        session.send({"op": "transcribe", "id": 13, "sample_rate": PCM_RATE}, b"")
        empty = session.response()
        case(
            "empty payload",
            empty.get("op") == "error" and empty.get("code") == "bad_audio" and empty.get("id") == 13,
            repr(empty),
        )
        transcribe(session, clips[0][1], 14)
        code = session.exit_code()
        case("transcribe after bad_audio", code == 0, f"status={code} stderr={session.err_text()!r}")
    except (ProtocolError, TimeoutError, OSError) as exc:
        case("bad_audio sequence", False, str(exc))
        if session:
            session.kill()
    session = None

    eof = Session(command, 30)
    try:
        code = eof.exit_code()
        case("EOF exits 0", code == 0, f"status={code} stderr={eof.err_text()!r}")
    except (TimeoutError, subprocess.TimeoutExpired, OSError) as exc:
        case("EOF exits 0", False, str(exc))
        eof.kill()

    try:
        session = hello_session()
        session.send({"op": "shutdown"})
        code = session.exit_code(close_stdin=False, timeout=30)
        case("shutdown exits 0", code == 0, f"status={code} stderr={session.err_text()!r}")
    except (ProtocolError, TimeoutError, OSError, subprocess.TimeoutExpired) as exc:
        case("shutdown exits 0", False, str(exc))
        if session:
            session.kill()
    return 0 if failures == 0 else 1


def pack_raw(header):
    return struct.pack("<I", len(header)) + header + struct.pack("<I", 0)


def write_wav(path, pcm):
    data_size = len(pcm)
    header = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF",
        36 + data_size,
        b"WAVE",
        b"fmt ",
        16,
        1,
        1,
        PCM_RATE,
        PCM_RATE * 2,
        2,
        16,
        b"data",
        data_size,
    )
    # A chunk the parser must skip, placed before data, is prepended by rewriting.
    junk = b"LIST" + struct.pack("<I", 4) + b"test"
    body = header[:12] + junk + header[12:]
    riff_size = len(body) - 8 + data_size
    body = body[:4] + struct.pack("<I", riff_size) + body[8:]
    with open(path, "wb") as handle:
        handle.write(body + pcm)


def self_test():
    ok = framing_tests()
    with tempfile.TemporaryDirectory() as tmp:
        model = os.path.join(tmp, "model")
        os.mkdir(model)
        pcm_a = struct.pack("<" + "h" * 8000, *([0] * 8000))
        pcm_b = struct.pack("<" + "h" * 8000, *([1000] * 8000))
        first = os.path.join(tmp, "a.wav")
        second = os.path.join(tmp, "b.wav")
        write_wav(first, pcm_a)
        write_wav(second, pcm_b)
        # Confirm the junk chunk is skipped and both clips parse.
        if read_wav_pcm(first) != pcm_a or read_wav_pcm(second) != pcm_b:
            ok &= check("wav parser", False, "roundtrip mismatch")
        else:
            ok &= check("wav parser", True)
        command = [sys.executable, os.path.abspath(__file__), "--fake-helper"]
        suite = run_suite(command, model, [first, second])
    return 0 if ok and suite == 0 else 1


def fake_reject(code, message, status, request_id=None):
    obj = {"op": "error", "code": code, "message": message}
    if request_id is not None:
        obj["id"] = request_id
    sys.stdout.buffer.write(pack(obj))
    sys.stdout.buffer.flush()
    if status != 0:
        raise SystemExit(status)


def fake_frame(reader):
    try:
        return reader.frame()
    except ProtocolError:
        fake_reject("protocol", "The frame is not valid protocol.", 2)


def fake_transcribe(obj, payload):
    request_id = obj.get("id")
    if type(request_id) is not int or request_id < 0:
        fake_reject("protocol", "transcribe requires an unsigned id", 2)
        return
    if obj.get("sample_rate") != PCM_RATE:
        fake_reject("bad_audio", "sample_rate must be 16000", 0, request_id)
        return
    if not payload or len(payload) % 2:
        fake_reject("bad_audio", "Audio must be non-empty 16-bit PCM.", 0, request_id)
        return
    if len(payload) // 2 > 19_200_000:
        fake_reject("bad_audio", "Audio is longer than 20 minutes.", 0, request_id)
        return
    sys.stdout.buffer.write(
        pack(
            {
                "op": "result",
                "id": request_id,
                "text": "ok",
                "audio_ms": audio_ms(payload),
                "compute_ms": 1,
            }
        )
    )
    sys.stdout.buffer.flush()


def fake_helper():
    if "--version" in sys.argv:
        sys.stdout.write(json.dumps({"helper": "fake", "protocol": 1}) + "\n")
        return 0
    reader = Reader(sys.stdin.buffer.read)
    frame = fake_frame(reader)
    if frame is None:
        return 0
    obj, payload = frame
    op = obj["op"]
    if op == "shutdown":
        if payload:
            fake_reject("protocol", "shutdown payload must be empty", 2)
        return 0
    if op != "hello":
        fake_reject("protocol", "The first request must be hello.", 2)
    if payload or obj.get("protocol") != 1:
        fake_reject("protocol", "protocol must be 1", 1)
    model_dir = obj.get("model_dir")
    if not isinstance(model_dir, str) or not model_dir.startswith("/") or not os.path.isdir(model_dir):
        fake_reject("model_missing", "The model directory does not exist.", 1)
    sys.stdout.buffer.write(pack({"op": "progress", "stage": "loading"}))
    sys.stdout.buffer.write(
        pack({"op": "ready", "protocol": 1, "helper": "fake", "load_ms": 0})
    )
    sys.stdout.buffer.flush()
    while True:
        frame = fake_frame(reader)
        if frame is None:
            return 0
        obj, payload = frame
        if obj["op"] == "shutdown":
            if payload:
                fake_reject("protocol", "shutdown payload must be empty", 2)
            return 0
        if obj["op"] != "transcribe":
            fake_reject("protocol", "Unknown op.", 2)
        fake_transcribe(obj, payload)


def main(argv):
    if "--fake-helper" in argv:
        return fake_helper()
    if "--self-test" in argv:
        return self_test()
    if len(argv) < 3:
        print("usage: test_protocol.py <helper> <model_dir> <wav>...", file=sys.stderr)
        print("       test_protocol.py --self-test", file=sys.stderr)
        return 2
    return run_suite([argv[0]], argv[1], argv[2:])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
