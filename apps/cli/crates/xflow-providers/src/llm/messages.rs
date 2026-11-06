//! Anthropic Messages.
use super::{json, malformed, stream_error, tool_args, ChatRequest, Event, Frame, Message};
use super::{LlmError, Stop, ToolCall};
use serde_json::{json, Value};

pub const VERSION: &str = "2023-06-01";

pub fn body(request: &ChatRequest, model: &str) -> Value {
    // The API rejects tool blocks in a request without tools, which is the forced
    // final answer after the hop limit: there the exchange becomes plain text.
    let plain = request.tools.is_empty();
    let mut messages: Vec<Value> = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        match message {
            Message::User(text) => messages.push(json!({"role":"user","content":text})),
            Message::Assistant {
                text, tool_calls, ..
            } => {
                let mut blocks = Vec::with_capacity(tool_calls.len() + 1);
                let mut text = text.clone();
                if plain {
                    for c in tool_calls {
                        text.push_str(&format!("\n[called {} {}]", c.name, c.arguments));
                    }
                }
                if !text.trim().is_empty() {
                    blocks.push(json!({"type":"text","text":text}));
                }
                if !plain {
                    blocks.extend(tool_calls.iter().map(
                        |c| json!({"type":"tool_use","id":c.id,"name":c.name,"input":c.arguments}),
                    ));
                }
                if !blocks.is_empty() {
                    messages.push(json!({"role":"assistant","content":blocks}));
                }
            }
            Message::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                let block = if plain {
                    json!({"type":"text","text":format!("[tool result] {content}")})
                } else {
                    json!({"type":"tool_result","tool_use_id":call_id,"content":content,"is_error":is_error})
                };
                // Results answering one assistant turn share one user message.
                if let Some(blocks) = messages
                    .last_mut()
                    .filter(|m| m["role"] == "user")
                    .and_then(|m| m["content"].as_array_mut())
                {
                    blocks.push(block);
                } else {
                    messages.push(json!({"role":"user","content":[block]}));
                }
            }
        }
    }
    let mut body = json!({"model":model,"stream":true,"max_tokens":request.max_output_tokens,"messages":messages});
    if !request.system.is_empty() {
        body["system"] = json!(request.system);
    }
    if !plain {
        body["tools"] = request
            .tools
            .iter()
            .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.input_schema}))
            .collect();
    }
    body
}

#[derive(Default)]
struct Block {
    id: String,
    name: String,
    arguments: String,
    tool: bool,
}
#[derive(Default)]
pub struct Parser {
    block: Block,
    called: bool,
    broken: bool,
    input: u64,
    cached: u64,
    output: u64,
    stop: Option<Stop>,
}
impl Parser {
    fn usage(&mut self, usage: &Value) {
        // `input` reports the whole prompt, as the other wires do.
        if usage["input_tokens"].is_u64() {
            self.input = usage["input_tokens"].as_u64().unwrap_or(0)
                + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                + usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
            self.cached = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
        }
        // Cumulative, so the latest value replaces the earlier one.
        if let Some(output) = usage["output_tokens"].as_u64() {
            self.output = output;
        }
    }
    pub fn frame(&mut self, frame: &Frame, out: &mut Vec<Event>) -> Result<bool, LlmError> {
        let value = json(&frame.data)?;
        let kind = if frame.event.is_empty() {
            value["type"].as_str().unwrap_or_default()
        } else {
            &frame.event
        };
        match kind {
            "message_start" => self.usage(&value["message"]["usage"]),
            "content_block_start" => {
                let block = &value["content_block"];
                self.block = Block {
                    id: block["id"].as_str().unwrap_or_default().to_owned(),
                    name: block["name"].as_str().unwrap_or_default().to_owned(),
                    arguments: String::new(),
                    tool: block["type"] == "tool_use",
                };
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = delta["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(Event::Text(text.to_owned()));
                        }
                    }
                    Some("input_json_delta") => {
                        // The first `partial_json` may be "".
                        self.block
                            .arguments
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let block = std::mem::take(&mut self.block);
                if block.tool {
                    match tool_args(&block.arguments).filter(|_| !block.name.is_empty()) {
                        Some(arguments) => {
                            self.called = true;
                            out.push(Event::ToolCall(ToolCall {
                                id: block.id,
                                name: block.name,
                                arguments,
                            }));
                        }
                        // Only `stop_reason` can tell a length cut from a bad model reply.
                        None => self.broken = true,
                    }
                }
            }
            "message_delta" => {
                self.usage(&value["usage"]);
                self.stop = value["delta"]["stop_reason"].as_str().map(|r| match r {
                    "max_tokens" => Stop::Length,
                    "tool_use" => Stop::ToolUse,
                    _ => Stop::End,
                });
            }
            "message_stop" => {
                let stop = self.stop.unwrap_or(Stop::End);
                if self.broken && stop != Stop::Length {
                    return Err(malformed());
                }
                out.push(Event::Usage {
                    input: self.input,
                    output: self.output,
                    cached: self.cached,
                });
                out.push(Event::Done(if self.called && stop != Stop::Length {
                    Stop::ToolUse
                } else {
                    stop
                }));
                return Ok(true);
            }
            "error" => return Err(stream_error(&value)),
            _ => {}
        }
        Ok(false)
    }
}
