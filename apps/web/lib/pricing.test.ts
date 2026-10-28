import { describe, expect, test } from "bun:test";
import { audioCostMicros, estimateInputTokens, FAIR_USE, tokenCostMicros } from "./pricing";
import { MAX_AUDIO_BYTES, MAX_CLEANUP_USER_BYTES, MAX_COMPLETION_TOKENS } from "./groq";

describe("provider cost model", () => {
  test("audio is billed per second with a 10 s minimum", () => {
    expect(audioCostMicros("whisper-large-v3-turbo", 3_600_000)).toBe(40_000);
    expect(audioCostMicros("whisper-large-v3-turbo", 2_000)).toBe(audioCostMicros("whisper-large-v3-turbo", 10_000));
    expect(audioCostMicros("whisper-large-v3", 3_600_000)).toBe(111_000);
  });

  test("tokens are billed per side", () => {
    expect(tokenCostMicros("qwen/qwen3.8-27b", 1_000_000, 0)).toBe(800_000);
    expect(tokenCostMicros("qwen/qwen3.8-27b", 0, 1_000_000)).toBe(4_000_000);
    expect(tokenCostMicros("qwen/qwen3.8-27b", 200, 60)).toBe(400);
  });

  test("unknown models have no price and can't be charged", () => {
    expect(() => audioCostMicros("whisper-1", 1000)).toThrow();
    expect(() => tokenCostMicros("llama-3.3-70b-versatile", 1, 1)).toThrow();
  });

  test("input estimate never undercounts ASCII text", () => {
    // ~4 chars per token for English; the estimate assumes 3.
    const text = "so like deploy the thing to staging and then restart xflowd ".repeat(20);
    expect(estimateInputTokens([{ content: text }])).toBeGreaterThan(text.length / 4);
  });
});

// Review M2: one Pro account must not be able to spend more than its fair-use budget.
describe("fair use bounds spend", () => {
  test("the monthly cap stays under the $5 price", () => {
    expect(FAIR_USE.monthlyMicros).toBeLessThan(5_000_000);
    expect(FAIR_USE.dailyMicros).toBeLessThanOrEqual(FAIR_USE.monthlyMicros);
  });

  test("the worst single request is a small fraction of the daily budget", () => {
    // Largest WAV the proxy accepts (16 kHz mono 16-bit) on the dearest model.
    const worstAudio = audioCostMicros("whisper-large-v3", ((MAX_AUDIO_BYTES - 44) / 32_000) * 1000);
    const worstCleanup = tokenCostMicros("qwen/qwen3.8-27b", estimateInputTokens([{ content: "x".repeat(MAX_CLEANUP_USER_BYTES * 2) }]), MAX_COMPLETION_TOKENS);
    expect(worstAudio).toBeLessThan(FAIR_USE.dailyMicros / 50);
    expect(worstCleanup).toBeLessThan(FAIR_USE.dailyMicros / 5);
  });

  test("a heavy but human day fits: 3 h of 15 s dictations with cleanup", () => {
    const perDictation = audioCostMicros("whisper-large-v3-turbo", 15_000) + tokenCostMicros("qwen/qwen3.8-27b", 200, 60);
    expect(perDictation * ((3 * 3600) / 15)).toBeLessThanOrEqual(FAIR_USE.dailyMicros * 1.05);
  });
});
