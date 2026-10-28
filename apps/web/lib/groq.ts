// Request validation for the hosted proxy. The accepted bodies are exactly what the Rust Groq
// provider sends (apps/cli/crates/xflow-providers: stt.rs multipart, lib.rs chat JSON), so the CLI only
// swaps the base URL and the bearer token. Anything else is rejected rather than forwarded.

export const GROQ_BASE_URL = "https://api.groq.com/openai/v1";

// Vercel caps a function request body at 4.5 MB. 16 kHz mono 16-bit WAV is 32 KB/s, so this is
// ~135 s of WAV, or far longer as FLAC/Opus.
export const MAX_AUDIO_BYTES = 4_400_000;
export const MAX_CLEANUP_BODY_BYTES = 320 * 1024;

export const STT_MODELS = ["whisper-large-v3-turbo", "whisper-large-v3"] as const;
export const CLEANUP_MODELS = ["qwen/qwen3.8-27b", "llama-3.1-8b-instant", "llama-3.3-70b-versatile"] as const;
const RESPONSE_FORMATS = ["json", "verbose_json", "text"];
const REASONING_EFFORTS = ["none", "default", "low", "medium", "high"];
const AUDIO_EXTENSIONS: Record<string, string> = {
  wav: "audio/wav",
  flac: "audio/flac",
  mp3: "audio/mpeg",
  mpeg: "audio/mpeg",
  mpga: "audio/mpeg",
  m4a: "audio/mp4",
  mp4: "audio/mp4",
  ogg: "audio/ogg",
  opus: "audio/ogg",
  webm: "audio/webm",
};

export class BadRequest extends Error {
  constructor(
    message: string,
    readonly status = 400,
  ) {
    super(message);
  }
}

export type TranscribeRequest = { form: FormData; model: string; audioBytes: number; audioMs: number };

export async function transcribeRequest(input: FormData): Promise<TranscribeRequest> {
  const allowed = new Set(["model", "file", "response_format", "language", "prompt"]);
  for (const key of input.keys()) if (!allowed.has(key)) throw new BadRequest(`unsupported field: ${key.slice(0, 40)}`);

  const model = textField(input, "model") ?? STT_MODELS[0];
  if (!(STT_MODELS as readonly string[]).includes(model)) throw new BadRequest(`model must be one of ${STT_MODELS.join(", ")}`);

  const file = input.get("file");
  if (!(file instanceof Blob) || input.getAll("file").length !== 1) throw new BadRequest("exactly one audio file is required");
  if (file.size === 0) throw new BadRequest("audio file is empty");
  if (file.size > MAX_AUDIO_BYTES) throw new BadRequest(`audio exceeds ${MAX_AUDIO_BYTES} bytes`, 413);
  const name = file instanceof File && file.name ? file.name : "dictation.wav";
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const mime = AUDIO_EXTENSIONS[ext];
  if (!mime) throw new BadRequest("unsupported audio container");

  const form = new FormData();
  form.set("model", model);
  form.set("file", new File([file], `dictation.${ext}`, { type: mime }));

  const responseFormat = textField(input, "response_format");
  if (responseFormat !== null) {
    if (!RESPONSE_FORMATS.includes(responseFormat)) throw new BadRequest("unsupported response_format");
    form.set("response_format", responseFormat);
  }
  const language = textField(input, "language");
  if (language !== null) {
    if (!/^[a-z]{2}$/.test(language)) throw new BadRequest("language must be a lowercase ISO-639-1 code");
    form.set("language", language);
  }
  const prompt = textField(input, "prompt");
  if (prompt !== null) {
    if (new TextEncoder().encode(prompt).length > 1024) throw new BadRequest("prompt exceeds 1024 bytes");
    form.set("prompt", prompt);
  }

  const audioMs = ext === "wav" ? wavDurationMs(new Uint8Array(await file.slice(0, 4096).arrayBuffer()), file.size) : 0;
  return { form, model, audioBytes: file.size, audioMs };
}

function textField(form: FormData, key: string): string | null {
  const values = form.getAll(key);
  if (values.length === 0) return null;
  const value = values[0];
  if (values.length > 1 || typeof value !== "string") throw new BadRequest(`${key} must be a single text field`);
  return value;
}

export type CleanupRequest = { body: Record<string, unknown>; model: string };

export function cleanupRequest(input: unknown): CleanupRequest {
  if (typeof input !== "object" || input === null || Array.isArray(input)) throw new BadRequest("body must be a JSON object");
  const raw = input as Record<string, unknown>;
  const allowed = new Set(["model", "stream", "messages", "reasoning_effort", "temperature", "max_completion_tokens"]);
  for (const key of Object.keys(raw)) if (!allowed.has(key)) throw new BadRequest(`unsupported field: ${key.slice(0, 40)}`);

  const model = raw.model;
  if (typeof model !== "string" || !(CLEANUP_MODELS as readonly string[]).includes(model)) {
    throw new BadRequest(`model must be one of ${CLEANUP_MODELS.join(", ")}`);
  }
  if (raw.stream !== undefined && raw.stream !== false) throw new BadRequest("stream must be false");

  const messages = raw.messages;
  if (!Array.isArray(messages) || messages.length === 0 || messages.length > 8) throw new BadRequest("messages must hold 1–8 entries");
  const clean = messages.map((m) => {
    if (typeof m !== "object" || m === null) throw new BadRequest("invalid message");
    const { role, content } = m as Record<string, unknown>;
    if (role !== "system" && role !== "user" && role !== "assistant") throw new BadRequest("invalid message role");
    if (typeof content !== "string") throw new BadRequest("message content must be text");
    return { role, content };
  });

  const body: Record<string, unknown> = { model, stream: false, messages: clean };
  if (raw.reasoning_effort !== undefined) {
    if (typeof raw.reasoning_effort !== "string" || !REASONING_EFFORTS.includes(raw.reasoning_effort)) {
      throw new BadRequest("unsupported reasoning_effort");
    }
    body.reasoning_effort = raw.reasoning_effort;
  }
  if (raw.temperature !== undefined) {
    if (typeof raw.temperature !== "number" || !(raw.temperature >= 0 && raw.temperature <= 2)) throw new BadRequest("temperature must be 0–2");
    body.temperature = raw.temperature;
  }
  if (raw.max_completion_tokens !== undefined) {
    const n = raw.max_completion_tokens;
    if (typeof n !== "number" || !Number.isInteger(n) || n < 1 || n > 32768) throw new BadRequest("max_completion_tokens must be 1–32768");
    body.max_completion_tokens = n;
  }
  return { body, model };
}

/** Duration of a PCM WAV from its header; 0 if the header can't be read. */
export function wavDurationMs(head: Uint8Array, totalBytes: number): number {
  const view = new DataView(head.buffer, head.byteOffset, head.byteLength);
  const tag = (at: number) => String.fromCharCode(...head.subarray(at, at + 4));
  if (head.length < 12 || tag(0) !== "RIFF" || tag(8) !== "WAVE") return 0;
  let byteRate = 0;
  for (let at = 12; at + 8 <= head.length; ) {
    const id = tag(at);
    const size = view.getUint32(at + 4, true);
    if (id === "fmt " && at + 16 <= head.length) byteRate = view.getUint32(at + 16, true);
    if (id === "data") {
      if (!byteRate) return 0;
      // Streaming writers leave the size as 0 or 0xffffffff; fall back to the file length.
      const dataBytes = size === 0 || size === 0xffffffff ? totalBytes - at - 8 : Math.min(size, totalBytes - at - 8);
      return Math.max(0, Math.round((dataBytes / byteRate) * 1000));
    }
    at += 8 + size + (size % 2);
  }
  return 0;
}

/** Token counts from a chat-completions response, if present. */
export function chatUsage(body: string): { inputTokens: number; outputTokens: number } {
  try {
    const usage = (JSON.parse(body) as { usage?: { prompt_tokens?: unknown; completion_tokens?: unknown } }).usage;
    const n = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? Math.max(0, Math.round(v)) : 0);
    return { inputTokens: n(usage?.prompt_tokens), outputTokens: n(usage?.completion_tokens) };
  } catch {
    return { inputTokens: 0, outputTokens: 0 };
  }
}
