import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { bearerToken, isApiTokenShape, isDeviceCodeShape, newApiToken, newDeviceCode, newUserCode, normalizeUserCode, sha256 } from "./tokens";

describe("api tokens", () => {
  test("stores only a SHA-256 hash; the hash matches the token", () => {
    const { token, hash, prefix } = newApiToken();
    expect(token).toMatch(/^xf_[A-Za-z0-9_-]{43}$/);
    expect(hash).toBe(createHash("sha256").update(token).digest("hex"));
    expect(hash).toHaveLength(64);
    expect(hash).not.toContain(token.slice(3));
    expect(token.startsWith(prefix)).toBe(true);
    expect(prefix).toHaveLength(10);
  });

  test("tokens are unique", () => {
    const seen = new Set(Array.from({ length: 1000 }, () => newApiToken().token));
    expect(seen.size).toBe(1000);
  });

  test("sha256 is stable hex", () => {
    expect(sha256("abc")).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  });

  test("bearer parsing accepts only well-formed xflow tokens", () => {
    const { token } = newApiToken();
    expect(bearerToken(`Bearer ${token}`)).toBe(token);
    expect(bearerToken(`bearer ${token}`)).toBe(token);
    expect(bearerToken(null)).toBeNull();
    expect(bearerToken(token)).toBeNull();
    expect(bearerToken(`Bearer ${token}x`)).toBeNull();
    expect(bearerToken("Bearer gsk_live_abcdef")).toBeNull();
    expect(bearerToken(`Bearer ${token} extra`)).toBeNull();
    expect(isApiTokenShape(token)).toBe(true);
  });
});

describe("device and user codes", () => {
  test("device codes carry 256 bits and hash like tokens", () => {
    const { deviceCode, hash } = newDeviceCode();
    expect(isDeviceCodeShape(deviceCode)).toBe(true);
    expect(hash).toBe(sha256(deviceCode));
  });

  test("user codes are XXXX-XXXX without vowels or lookalikes and round-trip", () => {
    for (let i = 0; i < 500; i++) {
      const code = newUserCode();
      expect(code).toMatch(/^[BCDFGHJKMNPQRSTVWXZ]{4}-[BCDFGHJKMNPQRSTVWXZ]{4}$/);
      expect(normalizeUserCode(code)).toBe(code);
    }
  });

  test("normalizes what people type", () => {
    expect(normalizeUserCode("bcdf ghjk")).toBe("BCDF-GHJK");
    expect(normalizeUserCode(" BCDFGHJK ")).toBe("BCDF-GHJK");
    expect(normalizeUserCode("BCDF-GHJ")).toBeNull();
    expect(normalizeUserCode("BCDF-GHJA")).toBeNull();
    expect(normalizeUserCode("'; drop table--")).toBeNull();
  });
});
