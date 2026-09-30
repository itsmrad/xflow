# xflow-providers

`build_stt(&SttConfig, offline)` returns a non-streaming `SpeechToText` implementation. `build_transformer(&CleanupConfig, offline)` returns an optional `TextTransformer`; raw cleanup creates no client. `save_key(provider, secret)` asynchronously stores a key in the OS credential store under service `xflow`, account `provider`.

Implemented provider contracts, verified against official documentation on 2026-09-30:

| Provider | Request | Default model | Credentials |
| --- | --- | --- | --- |
| Groq | Multipart `file`, `model`, optional `language`, `prompt` | `whisper-large-v3-turbo` | `GROQ_API_KEY` / `groq` keyring account |
| OpenRouter | JSON `input_audio: {data: base64, format: "wav"}`, `model`, optional `language` | `openai/whisper-large-v3` | `OPENROUTER_API_KEY` / `openrouter` |
| OpenAI | OpenAI multipart transcription | `whisper-1` | `OPENAI_API_KEY` / `openai` |
| Custom | OpenAI multipart transcription; explicit full endpoint and model required | Required | Configured environment variable / `custom` |

[Groq's official STT documentation](https://console.groq.com/docs/speech-to-text) specifies direct attachment limits, multilingual models and Whisper's 224-token prompt budget. The implementation conservatively limits joined vocabulary hints to 224 UTF-8 bytes. Configured language must be a lowercase two-letter code; invalid language, oversized hints and unsupported OpenRouter hints fail when the STT provider is built, before recording. Per-request overrides are checked again after merging with configured hints. [OpenRouter's official transcription reference](https://openrouter.ai/docs/api/api-reference/stt/create-transcription) documents its dedicated `/audio/transcriptions` endpoint and JSON audio format. OpenRouter supports `keyterms` only on some models; this adapter rejects vocabulary hints rather than silently losing them or assuming model support. [OpenAI's audio API reference](https://platform.openai.com/docs/api-reference/audio/createTranscription) defines the multipart contract.

`stt.endpoint` is a **full endpoint URL**, not a base URL. `stt.api_key_env` overrides the default environment variable. A named provider uses its default environment variable or keyring account only on its canonical HTTPS host. Other remote hosts require an explicit `stt.api_key_env` and never fall back to the named provider's keyring account; loopback overrides may be anonymous. Environment variables take precedence over OS storage, and an invalid/empty present variable fails instead of falling back silently. API keys are never read at client construction time. Linux uses Secret Service (GNOME Keyring/KWallet); macOS uses Keychain; Windows uses Credential Manager. A Linux source build requires `pkg-config` and DBus development headers. A headless environment can use environment variables without opening the keyring.

Cleanup is a separate OpenAI-compatible chat completion client. Light/polished modes require an explicit full `cleanup.endpoint` and `cleanup.model`. The default `cleanup.api_key_env` value selects the matching variable for canonical OpenAI, Groq or OpenRouter hosts; for other hosts it selects no environment key. Set a different variable name explicitly for a custom service. Its keyring account is `groq`, `openrouter` or `openai` for those canonical API hosts, otherwise `cleanup`. Application/window identifiers and selected text are not sent. LLM cleanup can make semantic mistakes; the caller should retain the raw transcript locally when history is enabled.

Only HTTPS remote endpoints are accepted. HTTP is permitted for literal loopback addresses (`127.0.0.1`, `[::1]`), and offline mode permits only literal loopback endpoints. Hostnames including `localhost` are rejected in offline mode, so DNS cannot redirect a local request remotely. Redirects, proxies and automatic retries are disabled. Loopback custom STT and cleanup with no explicitly configured key permit anonymous requests. To authenticate a local endpoint, configure a key environment variable explicitly.

Audio is checked for malformed frames and non-finite samples, downmixed to mono, clamped, then encoded as 16-bit PCM WAV at the original sample rate. WAV files are limited to 25 MB; input PCM is capped at 32 million samples; JSON responses at 1 MiB; transcript and cleanup text at 256 KiB. Deadlines cover credential lookup, encoding, HTTP transfer and response reading: configurable for STT (1–300 seconds) and 30 seconds for cleanup. A timeout or disconnect is never automatically retried because processing/billing may already have happened. HTTP error bodies and JSON parse details are omitted to prevent provider-echoed keys/audio/text reaching diagnostics.

Streaming, Deepgram, arbitrary non-OpenAI HTTP/WebSocket schemas and local ML runtimes are extension points in core, not implemented adapters. Unknown providers fail explicitly.

Contract tests run against local mock HTTP sockets and need permission to bind loopback sockets in restricted environments. They do not exercise a real paid provider, microphone or unlocked OS keyring.
