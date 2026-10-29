import { expect, test } from "bun:test";
import { refuse } from "./http";

const upload = (bytes: number) =>
  new Request("http://localhost/api/v1/transcribe", { method: "POST", body: new Uint8Array(bytes) });

// Tester P3: answering before the upload is read reset keep-alive connections (5 of 10 failed).
test("refuse reads a body within the limit and keeps the connection", async () => {
  const res = await refuse(upload(137 * 1024), new Response("no", { status: 401 }), 4_000_000);
  expect(res.status).toBe(401);
  expect(res.headers.get("connection")).toBeNull();
});

test("refuse stops reading past the limit and closes the connection", async () => {
  const res = await refuse(upload(256 * 1024), new Response("big", { status: 413 }), 0);
  expect(res.headers.get("connection")).toBe("close");
});
