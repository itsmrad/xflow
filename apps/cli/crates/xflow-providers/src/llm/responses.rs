//! OpenAI Responses: stateless (`store: false`), so reasoning items are replayed.
use super::{json, malformed, stream_error, tool_args, ChatRequest, Event, Frame, Message};
use super::{LlmError, Stop, ToolCall};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn body(request: &ChatRequest, model: &str) -> Value {
    let mut input = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        match message {
            Message::User(text) => input.push(json!({"role":"user","content":text})),
            Message::Assistant {
                text,
                tool_calls,
                opaque,
            } => {
                input.extend(opaque.iter().cloned());
                if !text.is_empty() {
                    input.push(json!({"role":"assistant","content":text}));
                }
                input.extend(tool_calls.iter().map(|c| {
                    json!({"type":"function_call","call_id":c.id,"name":c.name,"arguments":c.arguments.to_string()})
                }));
            }
            Message::ToolResult {
                call_id,
                content,
                is_error,
            } => input.push(json!({"type":"function_call_output","call_id":call_id,
                "output":if *is_error { format!("Error: {content}") } else { content.clone() }})),
        }
    }
    // Encrypted reasoning is what makes a stateless replay possible.
    let mut body = json!({"model":model,"stream":true,"store":false,
        "include":["reasoning.encrypted_content"],
        "max_output_tokens":request.max_output_tokens,"input":input});
    if !request.system.is_empty() {
        body["instructions"] = json!(request.system);
    }
    if !request.tools.is_empty() {
        // `strict` would require every optional search field.
        body["tools"] = request.tools.iter()
            .map(|t| json!({"type":"function","name":t.name,"description":t.description,"parameters":t.input_schema,"strict":false}))
            .collect();
    }
    body
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    sent: bool,
}
#[derive(Default)]
pub struct Parser {
    calls: BTreeMap<u64, Call>,
}
impl Parser {
    pub fn frame(&mut self, frame: &Frame, out: &mut Vec<Event>) -> Result<bool, LlmError> {
        let value = json(&frame.data)?;
        let index = value["output_index"].as_u64().unwrap_or(0);
        let kind = if frame.event.is_empty() {
            value["type"].as_str().unwrap_or_default()
        } else {
            &frame.event
        };
        match kind {
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(text) = value["delta"].as_str().filter(|t| !t.is_empty()) {
                    out.push(Event::Text(text.to_owned()));
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = &value["item"];
                match item["type"].as_str() {
                    Some("function_call") => {
                        let call = self.calls.entry(index).or_default();
                        call.id = item["call_id"].as_str().unwrap_or_default().to_owned();
                        call.name = item["name"].as_str().unwrap_or_default().to_owned();
                        if kind.ends_with(".done") {
                            if let Some(arguments) = item["arguments"].as_str() {
                                call.arguments = arguments.to_owned();
                            }
                            emit(call, out)?;
                        }
                    }
                    Some("reasoning") if kind.ends_with(".done") => {
                        out.push(Event::Opaque(item.clone()));
                    }
                    _ => {}
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    self.calls
                        .entry(index)
                        .or_default()
                        .arguments
                        .push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                let call = self.calls.entry(index).or_default();
                if let Some(arguments) = value["arguments"].as_str() {
                    call.arguments = arguments.to_owned();
                }
                emit(call, out)?;
            }
            "response.completed" | "response.incomplete" => {
                let reason = value["response"]["incomplete_details"]["reason"].as_str();
                let cut = kind == "response.incomplete" && reason == Some("max_output_tokens");
                let mut called = false;
                for call in self.calls.values_mut() {
                    // A length cut may stop mid-JSON; that call simply did not happen.
                    if cut && !call.sent {
                        continue;
                    }
                    emit(call, out)?;
                    called = true;
                }
                let usage = &value["response"]["usage"];
                out.push(Event::Usage {
                    input: usage["input_tokens"].as_u64().unwrap_or(0),
                    output: usage["output_tokens"].as_u64().unwrap_or(0),
                    cached: usage["input_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                });
                out.push(Event::Done(if cut {
                    Stop::Length
                } else if called {
                    Stop::ToolUse
                } else {
                    Stop::End
                }));
                return Ok(true);
            }
            "response.failed" => return Err(stream_error(&value["response"])),
            "error" => return Err(stream_error(&value)),
            _ => {}
        }
        Ok(false)
    }
}
fn emit(call: &mut Call, out: &mut Vec<Event>) -> Result<(), LlmError> {
    if call.sent {
        return Ok(());
    }
    let arguments = tool_args(&call.arguments)
        .filter(|_| !call.id.is_empty() && !call.name.is_empty())
        .ok_or_else(malformed)?;
    call.sent = true;
    out.push(Event::ToolCall(ToolCall {
        id: call.id.clone(),
        name: call.name.clone(),
        arguments,
    }));
    Ok(())
}
