# xflow-providers

Bounded, non-streaming speech transcription and optional text cleanup. Models are
selectable; static catalogs are suggestions, not allowlists. Defaults were checked
against official documentation on **2026-10-01**. No real microphone, credentials,
paid inference or daemon was used in verification. Mock tests establish wire
contracts, not account availability or cloud transcription quality.

## Provider report

| Provider id | Default model | Wire protocol | Official documentation / verification |
| --- | --- | --- | --- |
| `groq` | `whisper-large-v3-turbo` | OpenAI multipart | [STT guide](https://console.groq.com/docs/speech-to-text); documented |
| `openai` | `gpt-transcribe` | Multipart, keyword/language arrays | [Audio reference](https://developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create); documented |
| `deepgram` | `nova-3` | Raw audio, Token auth, query hints | [Pre-recorded API](https://developers.deepgram.com/docs/pre-recorded-audio); documented |
| `assemblyai` | `universal-3-5-pro` | Upload → submit → poll | [Submit API](https://www.assemblyai.com/docs/pre-recorded-audio/api-reference/transcripts/submit); documented |
| `elevenlabs` | `scribe_v2` | Multipart, xi-api-key | [Scribe API](https://elevenlabs.io/docs/api-reference/speech-to-text/convert); documented |
| `gemini` | `gemini-3.5-transcribe` | generateContent, inline audio, VERBATIM | [Transcribe guide](https://ai.google.dev/gemini-api/docs/generate-content/transcribe); config documented, inline path inferred from generic Part schema and not live-tested |
| `mistral` | `voxtral-mini-latest` | Multipart, context_bias | [Audio API](https://docs.mistral.ai/api/endpoint/audio/transcriptions); documented |
| `openrouter` | `openai/whisper-large-v3` | JSON input_audio, base64 | [STT reference](https://openrouter.ai/docs/api/api-reference/stt/create-transcription); documented |
| `together` | `openai/whisper-large-v3` | OpenAI multipart, language=auto | [Transcription guide](https://docs.together.ai/docs/inference/transcription/overview); documented |
| `deepinfra` | `openai/whisper-large-v3-turbo` | OpenAI multipart | [API reference](https://docs.deepinfra.com/api-reference/audio/openai-audio-transcriptions); documented |
| `custom` | Explicit model | OpenAI multipart by default | Explicit endpoint required; compatibility depends on server |
| `local` | `Systran/faster-whisper-small` | OpenAI multipart, no key | [Speaches](https://speaches.ai/); default http://127.0.0.1:8000/v1/audio/transcriptions |

DeepInfra replaces Fireworks STT, which was
[deprecated on 2026-06-10](https://docs.fireworks.ai/updates/changelog).
Fireworks remains a cleanup preset. Legacy OpenAI `gpt-4o-transcribe`,
`gpt-4o-mini-transcribe` and `whisper-1` remain selectable with retirement notes.
Catalog flags describe provider capabilities; individual models may impose stricter
limits. Arbitrary newer model ids are accepted.

`stt.endpoint` is a **full URL**, not a base URL. `stt.protocol` accepts
`openai`, `openai-keywords`, `openrouter`, `deepgram`, `assemblyai`,
`assemblyai-sync`, `elevenlabs`, `gemini`, `mistral` and `whisper-cpp`.
AssemblyAI sync defaults to https://sync.assemblyai.com/transcribe and requires
80 ms–120 seconds of 16 kHz WAV; use async for longer clips. Async polling starts
at 100 ms and doubles to one second under one overall deadline; upload and job
submission are never repeated. For whisper.cpp, configure `local` with
`protocol = "whisper-cpp"`, a server model id and endpoint
http://127.0.0.1:8080/inference. XFlow does not download local models; preinstall
them to avoid the server's own first-request download latency.

Language hints use lowercase ISO-639-1 codes; omission requests auto-detection.
Together explicitly sends `auto`, since its omitted-language default is English.
Dedicated Gemini ASR maps twelve common codes to locales; other codes require
auto-detection. Vocabulary merges/deduplicates config and request hints before
validation. OpenRouter rejects hints because upstream model support differs;
Together Parakeet rejects ignored prompts. Whisper uses a conservative 224-byte
prompt ceiling. Mistral allows 100 terms without spaces/commas; AssemblyAI permits
100 terms / 8000 bytes; Deepgram uses a conservative 500-byte budget. ElevenLabs
allows up to 1000 terms, each under 50 characters / five words without reserved
characters; its documented keyterm surcharge applies.

## API and caller integration

- `stt_providers()`, `cleanup_providers()`, `find_provider(ProviderKind, id)`
  expose `ProviderInfo` / `ModelInfo`; factories retain existing signatures.
- `key_status(provider, api_key_env)` returns only `Environment(name)`,
  `Keyring`, `NotRequired` or `Missing`, never a secret.
  `save_key` / `delete_key` manage provider keyring accounts.
- `check_stt` / `check_cleanup` return `CheckReport { ok, latency_ms, detail }`.
  They use non-billing GET/HEAD account, token or model probes, never audio uploads.
  Success establishes acceptance of that request, not inference permission or
  credit balance. HEAD on an unknown custom path may test only reachability.
- Call `SpeechToText::warm()` when recording starts and retain the **same instance**
  for transcription. Its five-second ceiling covers credential resolution and a
  bounded response drain so the pool can reuse the connection. Warm failure should
  not prevent recording. Rebuilding on every stop loses client/key caches.
- `transcribe_file` decodes PCM/float WAV through the normal path; recognized MP3,
  M4A/MP4, OGG, FLAC, WebM and MPEG/MPGA containers pass through without transcoding.
  Whisper.cpp and AssemblyAI sync require WAV; DeepInfra permits documented MP3
  and WAV only. Custom/local compatible servers may support fewer containers.
  Gemini additionally checks its full inline JSON size.

## Cleanup presets

| cleanup.provider | Default model |
| --- | --- |
| openai | gpt-5.6-luna, reasoning disabled |
| groq | qwen/qwen3.8-27b, reasoning disabled |
| openrouter | google/gemini-3.5-flash-lite |
| together | meta-llama/Llama-3.3-70B-Instruct-Turbo |
| fireworks | accounts/fireworks/models/qwen3-8b, reasoning disabled |
| mistral | mistral-small-latest |
| deepinfra | meta-llama/Llama-3.3-70B-Instruct |
| gemini | gemini-3.5-flash-lite, minimal reasoning, OpenAI-compatible route |
| ollama | llama3.2:3b, preinstall it |
| custom | Explicit endpoint and model |

Presets fill missing endpoint/model/key defaults; explicit values override them.
[GPT-5.6 Luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna) replaces
the retiring Nano default. Groq's [catalog](https://console.groq.com/docs/models)
restricts listed Llama alternatives to enterprise accounts. Thinking controls
are set only for verified specific models per the
[Groq](https://console.groq.com/docs/reasoning),
[Fireworks](https://docs.fireworks.ai/api-reference/post-chatcompletions) and
[Gemini compatibility](https://ai.google.dev/gemini-api/docs/openai) references.
Unknown model overrides retain server defaults.

Light cleanup fixes punctuation/capitalization, fillers and backtracking while
preserving wording. Polished improves clarity; custom uses
`TransformRequest.instructions`. Authorized commands edit supplied text or
write new text when input is empty. Vocabulary spellings/code identifiers are
preserved in the prompt. App tone uses `app_id` only when `cleanup.app_context`
is enabled. The transcript is a separate user message explicitly treated as data,
which reduces injection risk without guaranteeing model behavior. Retain raw
text for recovery. Raw factory mode returns no transformer; command callers must
build an enabled transformer.

## Privacy and bounds

Remote endpoints require HTTPS; HTTP/offline endpoints require literal loopback
addresses, not DNS names. Local presets cannot target remote hosts. Redirects,
proxies and retries are disabled. Default cloud keys are resolved only for the
canonical HTTPS host. Remote overrides require an explicit environment variable
and cannot receive the named provider's keyring key. Anonymous loopback overrides
do not read default cloud keys. Invalid/empty present environment keys fail
without falling back to storage.

Each provider retains one HTTP/2-enabled reqwest client and a sensitive, zeroized
credential-header cache. HTTP 401/403, key save/delete and rebuilding invalidate
the cache. Errors omit echoed response bodies, URLs and parse details. PCM input
is capped at 32 million samples, uploads at 25 MB, responses at 1 MiB and returned
text at 256 KiB. Configurable 1–300-second deadlines cover credentials, encoding,
transfer, polling and response reads. Encoding/decoding run off the async
executor; already-running encoding cannot be cancelled by timeout, but its result
is not uploaded after that deadline.

## Latency measurements

The 96-tap Blackman windowed-sinc resampler downmixes to 16 kHz mono with proper
anti-alias filtering. Tests cover 44.1/48/96 kHz amplitude, length and aliasing.
Pure-Rust FLAC is lossless after PCM16 quantization. It is adopted for documented
accepting protocols when WAV exceeds 4096 bytes and compression saves at least
10%. Groq, local, custom, OpenRouter, DeepInfra and AssemblyAI sync keep WAV.

Release-mode measurements on concatenated ALSA speech samples, 48 kHz stereo:

| Length | Original-rate mono WAV bytes / CPU | 16 kHz WAV bytes / resample+CPU | FLAC bytes / additional CPU |
| --- | --- | --- | --- |
| 2 s | 192044 / 0.665 ms | 64044 / 3.714 ms | 27991 / 2.199 ms |
| 10 s | 960044 / 4.385 ms | 320044 / 20.709 ms | 137671 / 10.680 ms |
| 56 s | 5376044 / 18.004 ms | 1792044 / 83.571 ms | 778317 / 44.874 ms |

A loopback mock imposed 40 ms connection delay and 1 MB/s body consumption.
Three runs on the 2-second fixture measured stop→text **252.19 ms before vs
40.17 ms after at the median**: before 247.42/252.19/254.65 ms; after
40.17/63.94/38.03 ms. Multipart upload bytes fell **192388 → 28507**.
Warm-up took about 41 ms during recording. The baseline used original-rate WAV
and a cold connection; the optimized path used 16 kHz, FLAC and a warmed connection.
This measures simulated upload/connection conditions, **not cloud inference**.
The socket test proves HTTP/1.1 warm/upload reuse of one TCP connection; it does
not establish TLS or HTTP/2 negotiation. Codec CPU increases and can outweigh
bandwidth savings on a very fast link. Native/incremental 16 kHz capture is the
audio-owner follow-up to remove stop-time resampling cost.

Reproduce with the mandatory shared gate:

```sh
/home/mrad/.cache/xflow-dev/bin/cargo run -p xflow-providers --release --example latency -- /path/to/speech48k_stereo.wav 2
```

The example without arguments uses a synthetic tone, whose compression differs
from the speech measurement. It binds only a loopback mock and joins its thread.

## Validation and remaining limits

`scripts/check.sh` passes at the provider branch based on contracts f510a2c:
workspace format, strict Clippy and 64 tests (41 provider tests), with one existing
platform session-bus test ignored. Tests cover each wire protocol, auth/fields,
redaction, deadlines, limits, file handling, key-cache invalidation, offline
boundaries, connection reuse, resampling and lossless compression.

Streaming, Files API uploads, automatic long-clip splitting and model downloads
remain outside scope. Model catalogs are static; account/model availability is
not validated through billable inference. Gemini's prompted fallback may
hallucinate on silence; the dedicated inline path needs an authorized account
test before claiming production validation. Custom endpoints can impose smaller
format/size limits. Repeated keyring mutation was not exercised on the user's
real credential store.
