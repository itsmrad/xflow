// Generates the CLI, config and provider references from the real binary and
// config/default.toml. Run from apps/web: `XFLOW_BIN=path/to/xflow bun scripts/gen-docs.ts`.
// Output is deterministic so CI can fail on a diff.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const BIN = process.env.XFLOW_BIN ?? "xflow";
const ROOT = join(import.meta.dir, "..");
const DOCS = join(ROOT, "content/docs");

const stripAnsi = (s: string) => s.replace(/\x1b\[[0-9;]*[A-Za-z]/g, "");
// Markdown prose (not code): keep MDX from reading `{`, `<` or `|` as syntax.
export const prose = (s: string) => s.replace(/([\\{}<>|])/g, "\\$1");

function run(args: string[]): string {
  const r = spawnSync(BIN, args, { encoding: "utf8", env: { ...process.env, NO_COLOR: "1" } });
  if (r.error) throw new Error(`cannot run ${BIN}: ${r.error.message}`);
  if (r.status !== 0) throw new Error(`${BIN} ${args.join(" ")} exited ${r.status}: ${r.stderr}`);
  return stripAnsi(r.stdout).replace(/[ \t]+$/gm, "").trimEnd();
}

function write(path: string, body: string) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, body.trimEnd() + "\n");
  console.log("wrote", path.slice(ROOT.length + 1));
}

// ---------- CLI reference ----------

/** Subcommand names from clap's `Commands:` section, skipping `help`. */
export function subcommands(help: string): string[] {
  const out: string[] = [];
  let inCommands = false;
  for (const line of help.split("\n")) {
    if (/^Commands:/.test(line)) inCommands = true;
    else if (inCommands && !line.trim()) break;
    else if (inCommands) {
      const name = /^ {2}([a-z][a-z0-9-]*)/.exec(line)?.[1];
      if (name && name !== "help") out.push(name);
    }
  }
  return out;
}

function cliReference(): string {
  const version = run(["--version"]).replace(/^xflow\s+/, "");
  const sections: string[] = [];
  const walk = (path: string[]) => {
    const help = run([...path, "--help"]);
    if (path.length) {
      const about = help.split("\n\n")[0]!.startsWith("Usage:") ? "" : help.split("\n\n")[0]!;
      sections.push(`${"#".repeat(Math.min(path.length + 1, 4))} xflow ${path.join(" ")}`);
      if (about) sections.push(prose(about.replace(/\n/g, " ")));
    } else {
      sections.push("## xflow");
    }
    sections.push("```text\n" + help + "\n```");
    for (const sub of subcommands(help)) walk([...path, sub]);
  };
  walk([]);
  return [
    "---",
    "title: CLI reference",
    "description: Every xflow command and flag, from the binary's own help.",
    "---",
    "",
    `Generated from \`xflow --help\` (xflow ${version}). Do not edit by hand: run \`bun scripts/gen-docs.ts\` in \`apps/web\`.`,
    "",
    "`--json`, `--quiet`, `--color auto|always|never` and `--config PATH` work on every command. JSON goes to stdout; errors are JSON objects on stderr with `error`, `hint` and `exit_code`.",
    "",
    ...sections.flatMap((s) => [s, ""]),
  ].join("\n");
}

// ---------- Config reference ----------

export interface ConfigKey {
  key: string;
  value: string | null; // null = commented out (unset by default)
  example?: string;
  description: string;
}
export interface ConfigSection {
  name: string; // "stt", or "[snippets]" for an array of tables
  array: boolean;
  commented: boolean; // whole section is an example in comments
  description: string;
  keys: ConfigKey[];
}

/** Split `value  # comment` at the first `#` outside a string. */
function splitComment(s: string): [string, string] {
  let quote = "";
  for (let i = 0; i < s.length; i++) {
    const c = s[i]!;
    if (quote) {
      if (c === "\\") i++;
      else if (c === quote) quote = "";
    } else if (c === '"' || c === "'") quote = c;
    else if (c === "#") return [s.slice(0, i).trim(), s.slice(i + 1).trim()];
  }
  return [s.trim(), ""];
}

/**
 * ponytail: a line parser for the flat layout of config/default.toml (one key per
 * line, no multi-line values). Swap in a real TOML parser if that file grows them.
 */
export function parseConfig(toml: string): { preamble: string[]; sections: ConfigSection[] } {
  const preamble: string[] = [];
  const sections: ConfigSection[] = [];
  let pending: string[] = [];
  let current: ConfigSection | undefined;
  for (const raw of toml.split("\n")) {
    const line = raw.trim();
    if (!line) {
      if (!current && pending.length) preamble.push(...pending);
      pending = [];
      continue;
    }
    const commented = line.startsWith("#");
    const body = commented ? line.replace(/^#\s?/, "") : line;
    const table = /^\[(\[)?([a-z_.]+)\]\]?$/.exec(body);
    if (table) {
      current = {
        name: table[2]!,
        array: !!table[1],
        commented,
        description: pending.join(" "),
        keys: [],
      };
      sections.push(current);
      pending = [];
      continue;
    }
    const kv = /^([a-z_][a-z0-9_]*)\s*=\s*(.+)$/.exec(body);
    if (kv && current) {
      const [value, comment] = splitComment(kv[2]!);
      const description = [...pending, comment].filter(Boolean).join(" ");
      current.keys.push(
        commented && !current.commented
          ? { key: kv[1]!, value: null, example: value, description }
          : { key: kv[1]!, value, description },
      );
      pending = [];
      continue;
    }
    if (commented) pending.push(body);
  }
  return { preamble, sections };
}

const cell = (s: string) => prose(s).replace(/`([^`]*)`/g, (_, code: string) => "`" + code.replace(/\\([\\{}<>])/g, "$1") + "`");

function configReference(toml: string, source: string): string {
  const { preamble, sections } = parseConfig(toml);
  const out = [
    "---",
    "title: Config reference",
    "description: Every setting in config.toml, its default and what it does.",
    "---",
    "",
    `Generated from \`${source}\`. Do not edit by hand: run \`bun scripts/gen-docs.ts\` in \`apps/web\`.`,
    "",
    "The file lives at `~/.config/xflow/config.toml`. `xflow config path` prints the exact location. Change a value with `xflow config set <key> <value>`; the daemon reloads it.",
    "",
    ...preamble.map(cell),
    "",
  ];
  for (const s of sections) {
    out.push(`## ${s.array ? `[[${s.name}]]` : `[${s.name}]`}`, "");
    if (s.description) out.push(cell(s.description), "");
    if (s.commented) {
      out.push("Not set by default. Example:", "", "```toml", `[[${s.name}]]`, ...s.keys.map((k) => `${k.key} = ${k.value}`), "```", "");
      continue;
    }
    out.push("| Key | Default | Description |", "| --- | --- | --- |");
    for (const k of s.keys) {
      const def = k.value === null ? "unset" : "`" + k.value.replace(/\|/g, "\\|") + "`";
      const desc = [k.description && cell(k.description), k.example && "e.g. `" + k.example.replace(/\|/g, "\\|") + "`"]
        .filter(Boolean)
        .join("; ");
      out.push(`| \`${s.name}.${k.key}\` | ${def} | ${desc} |`);
    }
    out.push("");
  }
  out.push("## Full default file", "", '```toml title="config.toml"', toml.trimEnd(), "```");
  return out.join("\n");
}

// ---------- Providers ----------

interface RawProvider {
  id: string;
  name: string;
  default_model: string;
  docs_url: string;
  key_url: string;
  env_var: string | null;
  local: boolean;
  vocabulary: boolean;
  models: { id: string; label: string; note: string }[];
}

function providers(configPath: string) {
  // A scratch copy keeps the user's config untouched.
  const dir = mkdtempSync(join(tmpdir(), "xflow-gen-"));
  const cfg = join(dir, "config.toml");
  copyFileSync(configPath, cfg);
  try {
    const raw = JSON.parse(run(["--config", cfg, "providers", "list", "--json"])) as RawProvider[];
    return raw.map((p) => {
      const selfHosted = p.local || p.id === "custom";
      return {
        id: p.id,
        name: p.name,
        kinds: ["STT"],
        defaultModel: p.default_model || null,
        local: p.local,
        vocabulary: p.vocabulary,
        envVar: p.env_var,
        docs: p.docs_url || null,
        keyUrl: p.key_url || null,
        href: selfHosted ? "/docs/providers/local-and-offline" : `/docs/providers/${p.id}`,
        models: p.models.filter((m) => m.id),
      };
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function providerPage(p: ReturnType<typeof providers>[number]): string {
  return [
    "---",
    `title: ${p.name}`,
    `description: Use ${p.name} for speech-to-text with your own key.`,
    "---",
    "",
    "Generated from `xflow providers list --json`. Do not edit by hand.",
    "",
    "## Set it up",
    "",
    ...(p.keyUrl ? [`Create a key in the [${p.name} console](${p.keyUrl}), then store it:`, ""] : []),
    "```sh",
    `xflow key set ${p.id}        # hidden input`,
    `xflow providers use ${p.id}`,
    `xflow providers test ${p.id} # checks the key; uploads no audio`,
    "```",
    "",
    ...(p.envVar ? [`Without a stored key, xflow reads \`${p.envVar}\` from the daemon's environment.`, ""] : []),
    '```toml title="config.toml"',
    "[stt]",
    `provider = "${p.id}"`,
    ...(p.defaultModel ? [`model = "${p.defaultModel}"   # the default; omit to follow it`] : []),
    "```",
    "",
    "## Models",
    "",
    "| Model | Name | Note |",
    "| --- | --- | --- |",
    ...p.models.map((m) => `| \`${m.id}\`${m.id === p.defaultModel ? " (default)" : ""} | ${prose(m.label)} | ${prose(m.note)} |`),
    "",
    p.vocabulary
      ? "Your dictionary words are sent as recognition hints with every recording."
      : `${p.name} takes no vocabulary hints, so dictionary words only apply through replacements after transcription.`,
    "",
    ...(p.docs ? [`Provider documentation: [${p.docs.replace(/^https?:\/\//, "")}](${p.docs})`] : []),
  ].join("\n");
}

if (import.meta.main) {
  const configPath = ["../../config/default.toml", "../cli/config/default.toml", "../../apps/cli/config/default.toml"]
    .map((p) => join(ROOT, p))
    .find(existsSync);
  if (!configPath) throw new Error("config/default.toml not found");
  write(join(DOCS, "reference/cli.mdx"), cliReference());
  write(join(DOCS, "reference/config.mdx"), configReference(readFileSync(configPath, "utf8"), "config/default.toml"));
  const list = providers(configPath);
  write(join(ROOT, "content/providers.json"), JSON.stringify(list, null, 2));
  for (const p of list) if (!p.href.endsWith("local-and-offline")) write(join(DOCS, `providers/${p.id}.mdx`), providerPage(p));
}
