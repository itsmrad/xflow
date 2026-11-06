# On-device speech helper protocol, version 1

`xflow-stt-coreml` is a separate executable that runs Parakeet TDT 0.6B v3
through Core ML. The daemon and CLI start it only for on-device work and talk
to it over the child's stdin and stdout. Nothing else: no sockets, no network,
no arguments or environment variables that carry audio or text.

## Framing

Every message in both directions is one frame. Integers are little-endian.

```
u32 header_len        1..=65536
header                header_len bytes, UTF-8 JSON object with an "op" field
u32 payload_len       0..=41943040 (40 MiB)
payload               payload_len bytes
```

A reader validates both lengths before allocating. A frame that breaks a
limit, has invalid JSON, or has an unknown `op` is a protocol error: the helper
writes an `error` frame with code `protocol` and exits with status 2.

## Requests (parent to helper)

`hello`, payload empty, always the first frame:

```json
{"op":"hello","protocol":1,"model_dir":"/absolute/path/to/model"}
```

`model_dir` holds `Preprocessor.mlmodelc`, `Encoder.mlmodelc`,
`Decoder.mlmodelc`, `JointDecisionv3.mlmodelc` and `parakeet_vocab.json`
directly. The helper loads them from that path only, with FluidAudio's
`ModelHub.offlineMode = true` set before any loader, and never downloads.
While loading it may send any number of
`{"op":"progress","stage":"loading"}` frames. The first load on a machine can
take about 20 seconds while Core ML prepares the model; later loads are under
a second.

Reply: `{"op":"ready","protocol":1,"helper":"<version>","load_ms":<n>}` or an
`error` frame. A `protocol` value other than 1 gets error code `protocol`.

`transcribe`, payload is mono 16 kHz signed 16-bit little-endian PCM:

```json
{"op":"transcribe","id":7,"sample_rate":16000}
```

`id` is a u64 chosen by the parent. `sample_rate` must be 16000. The payload
must be non-empty, an even number of bytes, and at most 19,200,000 samples
(20 minutes). Long audio is chunked inside the helper by FluidAudio.

Reply: `{"op":"result","id":7,"text":"...","audio_ms":<n>,"compute_ms":<n>}`
or `{"op":"error","id":7,"code":"...","message":"..."}`. Requests are handled
one at a time, in order. The parent sends the next `transcribe` only after the
previous reply.

`shutdown`, payload empty: the helper exits with status 0. End of stdin also
makes it exit with status 0.

## Errors

`{"op":"error","code":"<code>","message":"<plain English>"}`, with `id` when
it answers a `transcribe`. Codes: `protocol`, `unsupported_os`,
`model_missing`, `model_invalid`, `load_failed`, `bad_audio`,
`inference_failed`. After `hello` fails the helper exits with status 1.
`bad_audio` and `inference_failed` leave the helper running.

## Rules

- Diagnostics go to stderr only. Never write transcript text or audio to
  stderr, logs or files.
- The parent kills the helper to cancel work; the helper needs no cancel
  frame. It must not leave files behind.
- The parent passes no API keys or other secrets in the helper's environment.
- `xflow-stt-coreml --version` prints `{"helper":"<version>","protocol":1}`
  and exits 0 without loading anything.
