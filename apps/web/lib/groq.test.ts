import { describe, expect, test } from "bun:test";
import { BadRequest, chatUsage, cleanupRequest, MAX_AUDIO_BYTES, transcribeRequest, wavDurationMs } from "./groq";

function wav(seconds: number, rate = 16000): Uint8Array {
  const data = rate * 2 * seconds;
  const buf = new ArrayBuffer(44 + data);
  const v = new DataView(buf);
  const put = (at: number, s: string) => [...s].forEach((c, i) => v.setUint8(at + i, c.charCodeAt(0)));
  put(0, "RIFF");
  v.setUint32(4, 36 + data, true);
  put(8, "WAVE");
  put(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  put(36, "data");
  v.setUint32(40, data, true);
  return new Uint8Array(buf);
}

// What crates/xflow-providers/src/stt.rs sends to Groq today.
function rustForm(extra: Record<string, string> = {}): FormData {
  const form = new FormData();
  form.set("model", "whisper-large-v3-turbo");
  form.set("file", new File([wav(2) as BlobPart], "dictation.wav", { type: "audio/wav" }));
  form.set("response_format", "json");
  form.set("language", "en");
  form.set("prompt", "Kubernetes, kubectl, xflowd");
  for (const [k, v] of Object.entries(extra)) form.set(k, v);
  return form;
}

describe("transcribe request", () => {
  test("forwards the Rust provider's multipart body unchanged", async () => {
    const r = await transcribeRequest(rustForm());
    expect([...r.form.keys()].sort()).toEqual(["file", "language", "model", "prompt", "response_format"]);
    expect(r.form.get("model")).toBe("whisper-large-v3-turbo");
    expect(r.form.get("response_format")).toBe("json");
    expect(r.form.get("language")).toBe("en");
    expect(r.form.get("prompt")).toBe("Kubernetes, kubectl, xflowd");
    const file = r.form.get("file") as File;
    expect(file.name).toBe("dictation.wav");
    expect(file.type).toBe("audio/wav");
    expect(r.audioBytes).toBe(44 + 64000);
    expect(r.audioMs).toBe(2000);
  });

  test("only model and file are required", async () => {
    const form = new FormData();
    form.set("file", new File([new Uint8Array([1, 2, 3])], "dictation.flac"));
    const r = await transcribeRequest(form);
    expect(r.model).toBe("whisper-large-v3-turbo");
    expect((r.form.get("file") as File).type).toBe("audio/flac");
  });

  const rejects = async (form: FormData, status = 400) => {
    const err = await transcribeRequest(form).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(BadRequest);
    expect((err as BadRequest).status).toBe(status);
  };

  test("rejects unknown fields, models and languages", async () => {
    await rejects(rustForm({ temperature: "1" }));
    await rejects(rustForm({ model: "whisper-1" }));
    await rejects(rustForm({ language: "EN" }));
    await rejects(rustForm({ response_format: "srt" }));
    await rejects(rustForm({ prompt: "x".repeat(2000) }));
  });

  test("rejects missing, empty, oversized and non-audio files", async () => {
    const none = rustForm();
    none.delete("file");
    await rejects(none);
    const empty = rustForm();
    empty.set("file", new File([], "dictation.wav"));
    await rejects(empty);
    const big = rustForm();
    big.set("file", new File([new Uint8Array(MAX_AUDIO_BYTES + 1)], "dictation.wav"));
    await rejects(big, 413);
    const exe = rustForm();
    exe.set("file", new File([new Uint8Array(10)], "payload.exe"));
    await rejects(exe);
  });
});

describe("cleanup request", () => {
  // What crates/xflow-providers/src/lib.rs sends to Groq today.
  const rust = {
    model: "qwen/qwen3.8-27b",
    stream: false,
    messages: [
      { role: "system", content: "Correct punctuation and capitalization..." },
      { role: "user", content: "um so like deploy the thing" },
    ],
    reasoning_effort: "none",
  };

  test("forwards the Rust provider's JSON body unchanged", () => {
    expect(cleanupRequest(rust).body).toEqual(rust);
  });

  test("rejects streaming, unknown models, fields and roles", () => {
    expect(() => cleanupRequest({ ...rust, stream: true })).toThrow(BadRequest);
    expect(() => cleanupRequest({ ...rust, model: "gpt-5" })).toThrow(BadRequest);
    expect(() => cleanupRequest({ ...rust, tools: [] })).toThrow(BadRequest);
    expect(() => cleanupRequest({ ...rust, messages: [{ role: "tool", content: "x" }] })).toThrow(BadRequest);
    expect(() => cleanupRequest({ ...rust, messages: [] })).toThrow(BadRequest);
    expect(() => cleanupRequest({ ...rust, messages: [{ role: "user", content: [{ type: "image_url" }] }] })).toThrow(BadRequest);
    expect(() => cleanupRequest([])).toThrow(BadRequest);
  });

  test("reads token usage from a chat response", () => {
    expect(chatUsage('{"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":30}}')).toEqual({ inputTokens: 120, outputTokens: 30 });
    expect(chatUsage("not json")).toEqual({ inputTokens: 0, outputTokens: 0 });
  });
});

describe("wav duration", () => {
  test("reads PCM WAV headers", () => {
    const w = wav(3);
    expect(wavDurationMs(w.subarray(0, 4096), w.length)).toBe(3000);
  });
  test("falls back to the file length for streaming headers", () => {
    const w = wav(1);
    new DataView(w.buffer).setUint32(40, 0xffffffff, true);
    expect(wavDurationMs(w.subarray(0, 4096), w.length)).toBe(1000);
  });
  test("returns 0 for non-WAV data", () => {
    expect(wavDurationMs(new Uint8Array(100), 100)).toBe(0);
  });
});
