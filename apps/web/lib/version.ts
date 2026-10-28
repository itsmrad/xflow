import "server-only";
import { readFileSync } from "node:fs";
import path from "node:path";

/** Workspace version from apps/cli/Cargo.toml, at build time. */
export function cliVersion(): string {
  try {
    const toml = readFileSync(path.join(process.cwd(), "../cli/Cargo.toml"), "utf8");
    return toml.match(/\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"/)?.[1] ?? "0.1.0";
  } catch {
    return "0.1.0";
  }
}
