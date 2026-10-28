// Request validation for the hosted proxy. The accepted bodies are exactly what the Rust Groq
// provider sends (apps/cli/crates/xflow-providers: stt.rs multipart, lib.rs chat JSON), so the CLI only
// swaps the base URL and the bearer token. Anything else is rejected rather than forwarded.

export const GROQ_BASE_URL = "https://api.groq.com/openai/v1";

// Vercel caps a function request body at 4.5 MB. 16 kHz mono 16-bit WAV is 32 KB/s, so this is
// ~135 s of WAV, or far longer as FLAC/Opus.
export const MAX_AUDIO_BYTES = 4_400_000;
// Dictated text is small: 64 KiB of user text is over an hour of speech.
export const MAX_CLEANUP_BODY_BYTES = 128 * 1024;
export const MAX_CLEANUP_USER_BYTES = 64 * 1024;
// Cleanup output is about the size of its input. The provider counts the *requested* output
// against the account's output-tokens-per-minute limit, so ask for what the text needs:
// twice its estimated tokens, at least MIN_COMPLETION_TOKENS (commands that write new text).
export const MAX_COMPLETION_TOKENS = 4096;
export const MIN_COMPLETION_TOKENS = 512;

export const STT_MODELS = ["whisper-large-v3-turbo", "whisper-large-v3"] as const;
// Only the CLI default; the Llama presets need a provider enterprise account.
export const CLEANUP_MODELS = ["qwen/qwen3.8-27b"] as const;
const RESPONSE_FORMATS = ["json", "verbose_json", "text"];
const REASONING_EFFORTS = ["none", "default", "low", "medium", "high"];
// What the CLI uploads (encode_upload: WAV, or FLAC for longer clips). Both carry their
// exact duration in the header, so a request is charged before it is forwarded.
const AUDIO_EXTENSIONS: Record<string, string> = { wav: "audio/wav", flac: "audio/flac" };

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

  const head = new Uint8Array(await file.slice(0, 4096).arrayBuffer());
  const audioMs = ext === "wav" ? wavDurationMs(head, file.size) : flacDurationMs(head);
  if (audioMs <= 0) throw new BadRequest(`could not read the ${ext.toUpperCase()} header`);
  return { form, model, audioBytes: file.size, audioMs };
}

function textField(form: FormData, key: string): string | null {
  const values = form.getAll(key);
  if (values.length === 0) return null;
  const value = values[0];
  if (values.length > 1 || typeof value !== "string") throw new BadRequest(`${key} must be a single text field`);
  return value;
}

export type CleanupRequest = {
  body: Record<string, unknown>;
  model: string;
  messages: { role: string; content: string }[];
  maxCompletionTokens: number;
};

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
  const userBytes = clean.reduce((n, m) => n + (m.role === "system" ? 0 : new TextEncoder().encode(m.content).length), 0);
  if (userBytes > MAX_CLEANUP_USER_BYTES) throw new BadRequest(`text exceeds ${MAX_CLEANUP_USER_BYTES} bytes`, 413);

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
  const userTokens = Math.ceil(userBytes / 3);
  let maxCompletionTokens = Math.min(MAX_COMPLETION_TOKENS, Math.max(MIN_COMPLETION_TOKENS, 2 * userTokens));
  if (raw.max_completion_tokens !== undefined) {
    const n = raw.max_completion_tokens;
    if (typeof n !== "number" || !Number.isInteger(n) || n < 1 || n > MAX_COMPLETION_TOKENS) {
      throw new BadRequest(`max_completion_tokens must be 1–${MAX_COMPLETION_TOKENS}`);
    }
    maxCompletionTokens = Math.min(n, maxCompletionTokens);
  }
  // Always sent, so the reservation charged up front is a true upper bound.
  body.max_completion_tokens = maxCompletionTokens;
  return { body, model, messages: clean, maxCompletionTokens };
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

/** Duration of a FLAC stream from its STREAMINFO block; 0 if absent or unknown. */
export function flacDurationMs(head: Uint8Array): number {
  // "fLaC", then a metadata block header whose type (low 7 bits) is 0 = STREAMINFO.
  if (head.length < 8 + 18 || String.fromCharCode(...head.subarray(0, 4)) !== "fLaC" || (head[4]! & 0x7f) !== 0) return 0;
  const info = head.subarray(8);
  const rate = (info[10]! << 12) | (info[11]! << 4) | (info[12]! >> 4);
  const samples = (info[13]! & 0x0f) * 2 ** 32 + ((info[14]! << 24) >>> 0) + (info[15]! << 16) + (info[16]! << 8) + info[17]!;
  return rate > 0 && samples > 0 ? Math.round((samples / rate) * 1000) : 0;
}

/** Token counts from a chat-completions response; null when the response doesn't report them. */
export function chatUsage(body: string): { inputTokens: number; outputTokens: number } | null {
  try {
    const usage = (JSON.parse(body) as { usage?: { prompt_tokens?: unknown; completion_tokens?: unknown } }).usage;
    const n = (v: unknown) => (typeof v === "number" && Number.isFinite(v) && v >= 0 ? Math.round(v) : null);
    const inputTokens = n(usage?.prompt_tokens);
    const outputTokens = n(usage?.completion_tokens);
    return inputTokens === null || outputTokens === null ? null : { inputTokens, outputTokens };
  } catch {
    return null;
  }
}

/** Drops the provider's own fields (`x_groq`) from a JSON reply; xflow doesn't name its provider. */
export function publicBody(body: string): string {
  try {
    const value: unknown = JSON.parse(body);
    if (typeof value !== "object" || value === null || Array.isArray(value)) return body;
    const out = Object.fromEntries(Object.entries(value).filter(([k]) => !k.startsWith("x_groq")));
    return JSON.stringify(out);
  } catch {
    return body;
  }
}

/** A provider error message with the provider's name taken out. */
export function publicMessage(body: string): string {
  let message = "the request was rejected";
  try {
    const m = (JSON.parse(body) as { error?: { message?: unknown } }).error?.message;
    if (typeof m === "string" && m.trim()) message = m.slice(0, 500);
  } catch {
    // not JSON: keep the generic message
  }
  return message.replace(/groq(cloud)?/gi, "provider");
}
