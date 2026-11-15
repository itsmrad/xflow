import { expect, test } from "bun:test";
import { chatRequest, publicChatFrame, CHAT_MODEL } from "./chat";
import { tokenCostMicros, FAIR_USE } from "./pricing";
const request = (content = "Summarize the synthetic notes") => ({ stream: true, messages: [{ role: "user", content }] });
test("server chooses model and bounds output, including caller overrides", () => {
  const p = chatRequest({ ...request(), model: "expensive-model", max_tokens: 999999 });
  expect(p.body.model).toBe(CHAT_MODEL);
  expect(p.body.max_completion_tokens).toBe(2048);
  expect(p.reserveMicros).toBeLessThanOrEqual(tokenCostMicros(CHAT_MODEL, 32_000, 2048));
  expect(FAIR_USE.monthlyMicros).toBe(3_500_000);
});
test("UTF8, message framing, schemas and tool results all count against context", () => {
  for (const content of ["x".repeat(32000), "नमस्ते".repeat(3000)]) expect(() => chatRequest(request(content))).toThrow();
  expect(() => chatRequest({ ...request(), tools: [{ type: "function", function: { name: "search", description: "x".repeat(32000), parameters: {} } }] })).toThrow();
  const p = chatRequest({ stream: true, messages: [{ role: "assistant", content: "", tool_calls: [{ id: "c", type: "function", function: { name: "search", arguments: "{}" } }] }, { role: "tool", tool_call_id: "c", content: "synthetic result" }] });
  expect(p.body.messages[1]!.tool_call_id).toBe("c");
});
test("rejects invalid bodies and routing knobs", () => {
  for (const value of [null, [], { ...request(), stream: false }, { ...request(), provider: {} }, { ...request(), max_tokens: -1 }, { ...request(), messages: [{role:"developer",content:"bad"}] }]) expect(() => chatRequest(value)).toThrow();
});
test("SSE strips provider fields and never forwards provider error text", () => {
  const frame = publicChatFrame(JSON.stringify({ x_groq: {secret:"private"}, id:"private-id", choices:[{index:0,delta:{content:"hello", reasoning:"private-reasoning"}}] }));
  expect(frame).toContain("hello"); expect(frame).not.toContain("private");
  expect(() => publicChatFrame('{"error":{"message":"secret"}}')).toThrow();
  expect(publicChatFrame("[DONE]")).toBe("[DONE]");
});
