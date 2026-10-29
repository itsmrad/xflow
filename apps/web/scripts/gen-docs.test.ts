import { expect, test } from "bun:test";
import { parseConfig, subcommands } from "./gen-docs";

test("parseConfig keeps comments, unset keys and example sections", () => {
  const { preamble, sections } = parseConfig(`# xflow configuration.

[stt]
provider = "groq"            # \`xflow providers\` lists every provider
# model = "whisper"          # unset = provider default
# Seconds before giving up.
timeout_secs = 30
fillers = ["um", "a#b"]

# Voice snippets: say the trigger.
# [[snippets]]
# trigger = "my email"
`);
  expect(preamble).toEqual(["xflow configuration."]);
  const [stt, snippets] = sections;
  expect(stt!.keys).toEqual([
    { key: "provider", value: '"groq"', description: "`xflow providers` lists every provider" },
    { key: "model", value: null, example: '"whisper"', description: "unset = provider default" },
    { key: "timeout_secs", value: "30", description: "Seconds before giving up." },
    { key: "fillers", value: '["um", "a#b"]', description: "" },
  ]);
  expect(snippets).toMatchObject({ name: "snippets", array: true, commented: true, description: "Voice snippets: say the trigger." });
  expect(snippets!.keys).toEqual([{ key: "trigger", value: '"my email"', description: "" }]);
});

test("subcommands reads clap's Commands section and skips help", () => {
  const help = "About\n\nUsage: xflow key <COMMAND>\n\nCommands:\n  set     Store\n  remove  \n  help    Print this\n\nOptions:\n  -h  Help";
  expect(subcommands(help)).toEqual(["set", "remove"]);
});
