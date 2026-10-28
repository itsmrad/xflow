// Provider cost model for Pro's fair use. All amounts are micro-dollars (1e-6 USD), integers.
// List prices, October 2026; re-check when the provider changes them.

const AUDIO_PER_HOUR: Record<string, number> = {
  "whisper-large-v3-turbo": 40_000, // $0.04 / audio hour
  "whisper-large-v3": 111_000, // $0.111 / audio hour
};
// The provider bills at least 10 s per transcription request.
const MIN_BILLED_AUDIO_MS = 10_000;

const TOKENS_PER_MILLION: Record<string, { input: number; output: number }> = {
  "qwen/qwen3.8-27b": { input: 800_000, output: 4_000_000 }, // $0.80 in / $4.00 out per 1M
};

/**
 * Pro is $5/month "unlimited" for a person dictating. These caps bound what one account can
 * spend on the shared provider key: about break-even after payment fees at the monthly cap.
 * At ~15 s per dictation with cleanup (~570 µ$ each), the daily cap is ~700 dictations
 * (~3 h of speech) and the monthly cap ~7,000 (~29 h). Overridable without a deploy of code.
 */
export const FAIR_USE = {
  dailyMicros: usd(process.env.FAIR_USE_DAILY_USD, 0.4),
  monthlyMicros: usd(process.env.FAIR_USE_MONTHLY_USD, 4),
};

function usd(value: string | undefined, fallback: number): number {
  const n = value === undefined ? fallback : Number(value);
  return Math.round((Number.isFinite(n) && n > 0 ? n : fallback) * 1_000_000);
}

export function audioCostMicros(model: string, audioMs: number): number {
  const perHour = AUDIO_PER_HOUR[model];
  if (perHour === undefined) throw new Error(`no price for ${model}`);
  return Math.ceil((Math.max(audioMs, MIN_BILLED_AUDIO_MS) / 3_600_000) * perHour);
}

export function tokenCostMicros(model: string, inputTokens: number, outputTokens: number): number {
  const price = TOKENS_PER_MILLION[model];
  if (!price) throw new Error(`no price for ${model}`);
  return Math.ceil((inputTokens * price.input + outputTokens * price.output) / 1_000_000);
}

/** Conservative token count before the provider reports one: ~3 bytes per token, plus overhead per message. */
export function estimateInputTokens(messages: readonly { content: string }[]): number {
  return messages.reduce((n, m) => n + Math.ceil(new TextEncoder().encode(m.content).length / 3) + 8, 0);
}
