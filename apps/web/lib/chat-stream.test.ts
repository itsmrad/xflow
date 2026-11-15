import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
test("streaming contract with stubbed database and provider in an isolated process", () => {
  const child = spawnSync(process.execPath, ["test", "./test/stubs/chat.fixture.ts"], { cwd: new URL("..", import.meta.url), env: { NODE_ENV: "test", PATH: process.env.PATH }, encoding: "utf8" });
  expect(child.stdout + child.stderr).toContain("0 fail");
  expect(child.status).toBe(0);
});
