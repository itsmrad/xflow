//! OpenAI-compatible Chat Completions: OpenRouter and custom loopback servers.
use super::{json, malformed, stream_error, tool_args, ChatRequest, Event, Frame, Message};
use super::{LlmError, Stop, ToolCall};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn body(request: &ChatRequest, model: &str, openrouter: bool) -> Value {
    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    if !request.system.is_empty() {
        messages.push(json!({"role":"system","content":request.system}));
    }
    for message in &request.messages {
        messages.push(match message {
            Message::User(text) => json!({"role":"user","content":text}),
            Message::Assistant {
                text, tool_calls, ..
            } => {
                let mut item = json!({"role":"assistant","content":text});
                if !tool_calls.is_empty() {
                    item["tool_calls"] = tool_calls
                        .iter()
                        .map(|c| json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments.to_string()}}))
                        .collect();
                }
                item
            }
            Message::ToolResult {
                call_id,
                content,
                is_error,
            } => json!({"role":"tool","tool_call_id":call_id,
                "content":if *is_error { format!("Error: {content}") } else { content.clone() }}),
        });
    }
    let mut body = json!({"model":model,"stream":true,"stream_options":{"include_usage":true},
        "max_tokens":request.max_output_tokens,"messages":messages});
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter()
            .map(|t| json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.input_schema}}))
            .collect();
    }
    if openrouter {
        body["provider"] = json!({"data_collection":"deny"});
    }
    body
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}
#[derive(Default)]
pub struct Parser {
    calls: BTreeMap<u64, Call>,
    usage: Option<Event>,
    stop: Option<Stop>,
}
impl Parser {
    pub fn frame(&mut self, frame: &Frame, out: &mut Vec<Event>) -> Result<bool, LlmError> {
        if frame.data.trim() == "[DONE]" {
            self.finish(out)?;
            return Ok(true);
        }
        let value = json(&frame.data)?;
        if value.get("error").is_some_and(|e| !e.is_null()) {
            return Err(stream_error(&value));
        }
        if value["usage"].is_object() {
            let usage = &value["usage"];
            self.usage = Some(Event::Usage {
                input: usage["prompt_tokens"].as_u64().unwrap_or(0),
                output: usage["completion_tokens"].as_u64().unwrap_or(0),
                cached: usage["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0),
            });
        }
        // A usage chunk may have empty `choices`.
        let choice = &value["choices"][0];
        let reason = choice["finish_reason"].as_str();
        if reason == Some("error") {
            return Err(stream_error(choice));
        }
        if let Some(text) = choice["delta"]["content"].as_str() {
            if !text.is_empty() {
                out.push(Event::Text(text.to_owned()));
            }
        }
        for (position, delta) in choice["delta"]["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let index = delta["index"].as_u64().unwrap_or(position as u64);
            let call = self.calls.entry(index).or_default();
            let id = delta["id"].as_str().filter(|id| !id.is_empty());
            let fragment = delta["function"]["arguments"].as_str().unwrap_or_default();
            // A server repeating a finished entry must not corrupt its arguments.
            let repeated = id.is_some_and(|id| id == call.id)
                && serde_json::from_str::<Value>(&call.arguments).is_ok();
            if !repeated {
                call.arguments.push_str(fragment);
            }
            if let Some(id) = id {
                call.id = id.to_owned();
            }
            if let Some(name) = delta["function"]["name"].as_str() {
                if !name.is_empty() {
                    call.name = name.to_owned();
                }
            }
        }
        if let Some(reason) = reason {
            self.stop = Some(match reason {
                "length" => Stop::Length,
                "tool_calls" => Stop::ToolUse,
                _ => Stop::End,
            });
        }
        Ok(false)
    }
    /// A body may end without `[DONE]` once a finish reason has arrived.
    pub fn eof(&mut self, out: &mut Vec<Event>) -> Result<bool, LlmError> {
        if self.stop.is_some() {
            self.finish(out)?;
            return Ok(true);
        }
        Ok(false)
    }
    fn finish(&mut self, out: &mut Vec<Event>) -> Result<(), LlmError> {
        let cut = self.stop == Some(Stop::Length);
        let mut called = false;
        for (index, call) in std::mem::take(&mut self.calls) {
            match tool_args(&call.arguments).filter(|_| !call.name.is_empty()) {
                Some(arguments) => {
                    called = true;
                    out.push(Event::ToolCall(ToolCall {
                        id: if call.id.is_empty() {
                            format!("call_{index}")
                        } else {
                            call.id
                        },
                        name: call.name,
                        arguments,
                    }));
                }
                // A length cut may stop mid-JSON; that call simply did not happen.
                None if cut => {}
                None => return Err(malformed()),
            }
        }
        out.extend(self.usage.take());
        out.push(Event::Done(match self.stop {
            Some(Stop::Length) => Stop::Length,
            _ if called => Stop::ToolUse,
            _ => Stop::End,
        }));
        Ok(())
    }
}
