import { describe, expect, test } from "bun:test";
import {
  BadRequest,
  chatUsage,
  cleanupRequest,
  billedAudioMs,
  provablyNotSent,
  transcriptionReply,
  MAX_AUDIO_BYTES,
  MAX_COMPLETION_TOKENS,
  MIN_COMPLETION_TOKENS,
  publicBody,
  publicMessage,
  transcribeRequest,
  wavDurationMs,
} from "./groq";

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

// What apps/cli/crates/xflow-providers/src/stt.rs sends to Groq today.
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
    // Upstream always reports the billed duration; the client still gets the format it asked for.
    expect(r.form.get("response_format")).toBe("verbose_json");
    expect(r.responseFormat).toBe("json");
    expect(r.form.get("language")).toBe("en");
    expect(r.form.get("prompt")).toBe("Kubernetes, kubectl, xflowd");
    const file = r.form.get("file") as File;
    expect(file.name).toBe("dictation.wav");
    expect(file.type).toBe("audio/wav");
    expect(r.audioBytes).toBe(44 + 64000);
    expect(r.audioMs).toBe(2000);
  });

  test("only the file is required", async () => {
    const form = new FormData();
    form.set("file", new File([wav(1) as BlobPart], "dictation.wav"));
    const r = await transcribeRequest(form);
    expect(r.model).toBe("whisper-large-v3-turbo");
    expect(r.responseFormat).toBe("json");
    expect(r.audioMs).toBe(1000);
  });

  // Review N2: the charge must not trust a duration the client can forge.
  test("refuses FLAC and other compressed containers (their length can't be bounded)", async () => {
    for (const name of ["dictation.flac", "dictation.mp3", "dictation.ogg", "dictation.webm", "dictation.m4a"]) {
      const other = new FormData();
      other.set("file", new File([new Uint8Array(100)], name));
      await rejects(other);
    }
  });

  test("refuses WAV headers that aren't self-consistent 16-bit PCM", async () => {
    const forge = (edit: (v: DataView) => void) => {
      const w = wav(2);
      edit(new DataView(w.buffer));
      const f = new FormData();
      f.set("file", new File([w as BlobPart], "dictation.wav"));
      return f;
    };
    await rejects(forge((v) => v.setUint32(28, 32000 * 20, true))); // byteRate x20: would claim 1/20 the length
    await rejects(forge((v) => v.setUint16(34, 8, true))); // 8-bit
    await rejects(forge((v) => v.setUint16(20, 3, true))); // float
    await rejects(forge((v) => v.setUint32(24, 1000, true))); // 1 kHz
    await rejects(forge((v) => v.setUint16(22, 6, true))); // 6 channels
    const junk = new FormData();
    junk.set("file", new File([new Uint8Array(100)], "dictation.wav"));
    await rejects(junk);
  });

  test("a shrunken data-size field can't shorten the charge", async () => {
    const w = wav(5);
    new DataView(w.buffer).setUint32(40, 3200, true); // claims 0.1 s of data
    const f = new FormData();
    f.set("file", new File([w as BlobPart], "dictation.wav"));
    expect((await transcribeRequest(f)).audioMs).toBe(5000);
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
  // What apps/cli/crates/xflow-providers/src/lib.rs sends to Groq today.
  const rust = {
    model: "qwen/qwen3.8-27b",
    stream: false,
    messages: [
      { role: "system", content: "Correct punctuation and capitalization..." },
      { role: "user", content: "um so like deploy the thing" },
    ],
    reasoning_effort: "none",
  };

  test("forwards the Rust provider's JSON body, adding only an output cap sized to the text", () => {
    expect(cleanupRequest(rust).body).toEqual({ ...rust, max_completion_tokens: MIN_COMPLETION_TOKENS });
  });

  test("the output cap grows with the text, up to the maximum", () => {
    const withText = (n: number) => ({ ...rust, messages: [rust.messages[0]!, { role: "user", content: "x".repeat(n) }] });
    expect(cleanupRequest(withText(3000)).maxCompletionTokens).toBe(2000);
    expect(cleanupRequest(withText(60_000)).maxCompletionTokens).toBe(MAX_COMPLETION_TOKENS);
    // A client may ask for less, never more than the text warrants.
    expect(cleanupRequest({ ...withText(3000), max_completion_tokens: 100 }).maxCompletionTokens).toBe(100);
    expect(cleanupRequest({ ...withText(30), max_completion_tokens: 4000 }).maxCompletionTokens).toBe(MIN_COMPLETION_TOKENS);
  });

  // research-hosted #2: the CLI sends max_completion_tokens 8192 (xflow-providers); it must be clamped, not a 400.
  test("an over-limit output request is clamped to the cap, never refused", () => {
    const long = { ...rust, messages: [rust.messages[0]!, { role: "user", content: "x".repeat(60_000) }], max_completion_tokens: 8192 };
    expect(cleanupRequest(long).maxCompletionTokens).toBe(MAX_COMPLETION_TOKENS);
    expect(cleanupRequest(long).body.max_completion_tokens).toBe(MAX_COMPLETION_TOKENS);
    expect(cleanupRequest({ ...rust, max_completion_tokens: 8192 }).maxCompletionTokens).toBe(MIN_COMPLETION_TOKENS);
    for (const bad of [0, -1, 1.5, "8192", null]) expect(() => cleanupRequest({ ...rust, max_completion_tokens: bad })).toThrow(BadRequest);
  });

  // Review M2: a 310 KB prompt with max_completion_tokens 32768 on a 70B model was forwarded.
  test("bounds cost per request: models, text size, output tokens", () => {
    expect(() => cleanupRequest({ ...rust, model: "llama-3.3-70b-versatile" })).toThrow(BadRequest);
    expect(cleanupRequest({ ...rust, max_completion_tokens: 32768 }).body.max_completion_tokens).toBe(MIN_COMPLETION_TOKENS);
    expect(cleanupRequest({ ...rust, max_completion_tokens: 256 }).body.max_completion_tokens).toBe(256);
    const big = { ...rust, messages: [rust.messages[0]!, { role: "user", content: "x".repeat(70_000) }] };
    const err = (() => {
      try {
        cleanupRequest(big);
      } catch (e) {
        return e;
      }
    })();
    expect(err).toBeInstanceOf(BadRequest);
    expect((err as BadRequest).status).toBe(413);
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
    expect(chatUsage("not json")).toBeNull();
    expect(chatUsage('{"choices":[]}')).toBeNull();
  });

  test("replies don't name the provider", () => {
    expect(JSON.parse(publicBody('{"text":"hi","x_groq":{"id":"req_1"}}'))).toEqual({ text: "hi" });
    expect(publicBody("plain text")).toBe("plain text");
    expect(publicMessage('{"error":{"message":"Groq could not decode the file"}}')).toBe("provider could not decode the file");
    expect(publicMessage("<html>")).toBe("the request was rejected");
  });
});

describe("wav duration", () => {
  test("reads PCM WAV headers", () => {
    const w = wav(3);
    expect(wavDurationMs(w.subarray(0, 4096), w.length)).toBe(3000);
  });
  test("counts every byte after the data header, whatever its size says", () => {
    const w = wav(1);
    new DataView(w.buffer).setUint32(40, 0xffffffff, true);
    expect(wavDurationMs(w.subarray(0, 4096), w.length)).toBe(1000);
  });
  test("returns 0 for non-WAV data", () => {
    expect(wavDurationMs(new Uint8Array(100), 100)).toBe(0);
  });
});

describe("provider replies", () => {
  test("the billed duration is read from verbose_json", () => {
    expect(billedAudioMs('{"text":"hi","duration":600.0001}')).toBe(600_001);
    expect(billedAudioMs('{"text":"hi"}')).toBeNull();
    expect(billedAudioMs("nope")).toBeNull();
  });

  test("transcripts come back in the format the client asked for", () => {
    const upstream = '{"task":"transcribe","language":"english","duration":2.5,"text":" hi there","segments":[],"x_groq":{"id":"r"}}';
    expect(transcriptionReply(upstream, "json")).toEqual({ body: '{"text":" hi there"}', contentType: "application/json" });
    expect(transcriptionReply(upstream, "text")).toEqual({ body: " hi there", contentType: "text/plain; charset=utf-8" });
    const verbose = JSON.parse(transcriptionReply(upstream, "verbose_json").body);
    expect(verbose.duration).toBe(2.5);
    expect(verbose.x_groq).toBeUndefined();
  });
});

// Review N1: only a request that never left the server may be refunded.
describe("refund classification", () => {
  const err = (code?: string) => Object.assign(new TypeError("fetch failed"), code ? { cause: { code } } : {});
  test("connection never made: refund", () => {
    for (const code of ["ECONNREFUSED", "ENOTFOUND", "UND_ERR_CONNECT_TIMEOUT"]) expect(provablyNotSent(err(code))).toBe(true);
  });
  test("client hang-up, timeout or a reset mid-flight: keep the reservation", () => {
    expect(provablyNotSent(new DOMException("aborted", "AbortError"))).toBe(false);
    expect(provablyNotSent(new DOMException("timed out", "TimeoutError"))).toBe(false);
    expect(provablyNotSent(err("ECONNRESET"))).toBe(false);
    expect(provablyNotSent(err("UND_ERR_SOCKET"))).toBe(false);
    expect(provablyNotSent(err())).toBe(false);
  });
});
