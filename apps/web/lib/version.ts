import "server-only";
import { readFileSync } from "node:fs";
import path from "node:path";

/** Workspace version from Cargo.toml (root today, apps/cli after the monorepo move). Build time only. */
export function cliVersion(): string {
  for (const p of ["../../Cargo.toml", "../cli/Cargo.toml"]) {
    try {
      const toml = readFileSync(path.join(process.cwd(), p), "utf8");
      const v = toml.match(/\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"/)?.[1];
      if (v) return v;
    } catch {}
  }
  return "0.1.0";
}
