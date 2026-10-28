import { createHash, randomBytes, randomInt } from "node:crypto";

export const TOKEN_PREFIX = "xf_";
const TOKEN_RE = /^xf_[A-Za-z0-9_-]{43}$/;
const DEVICE_CODE_RE = /^[A-Za-z0-9_-]{43}$/;
// No vowels (no accidental words) and no 0/O, 1/I/L lookalikes.
const USER_CODE_ALPHABET = "BCDFGHJKMNPQRSTVWXZ";

export function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

/** A new opaque API token. Only its hash is stored. */
export function newApiToken(): { token: string; hash: string; prefix: string } {
  const token = TOKEN_PREFIX + randomBytes(32).toString("base64url");
  return { token, hash: sha256(token), prefix: token.slice(0, 10) };
}

export function isApiTokenShape(value: string): boolean {
  return TOKEN_RE.test(value);
}

export function newDeviceCode(): { deviceCode: string; hash: string } {
  const deviceCode = randomBytes(32).toString("base64url");
  return { deviceCode, hash: sha256(deviceCode) };
}

export function isDeviceCodeShape(value: string): boolean {
  return DEVICE_CODE_RE.test(value);
}

/** XXXX-XXXX from a 19-letter alphabet: ~34 bits, enough for a 10-minute, rate-limited code. */
export function newUserCode(): string {
  let code = "";
  for (let i = 0; i < 8; i++) code += USER_CODE_ALPHABET[randomInt(USER_CODE_ALPHABET.length)];
  return `${code.slice(0, 4)}-${code.slice(4)}`;
}

/** Accepts what a person types (any case, spaces, missing dash); null if it can't be a code. */
export function normalizeUserCode(input: string): string | null {
  const letters = input.toUpperCase().replace(/[\s-]/g, "");
  if (letters.length !== 8 || [...letters].some((c) => !USER_CODE_ALPHABET.includes(c))) return null;
  return `${letters.slice(0, 4)}-${letters.slice(4)}`;
}

/** Reads `Authorization: Bearer xf_...`; null unless it is well formed. */
export function bearerToken(header: string | null): string | null {
  const match = header?.match(/^Bearer\s+(\S+)$/i);
  const token = match?.[1];
  return token && isApiTokenShape(token) ? token : null;
}
