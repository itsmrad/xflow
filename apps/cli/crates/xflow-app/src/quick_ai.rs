//! A short, context-free voice turn over the shared model gateway.
use anyhow::{bail, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use xflow_core::{config::Config, ChatProvider};
use xflow_providers::llm::{self, ChatProvider as _, Event};

struct VoiceChat {
    gateway: llm::Gateway,
    provider: String,
    model: String,
}

pub(crate) fn build(config: &Config) -> Result<Arc<dyn ChatProvider>> {
    if matches!(config.ask.provider.as_str(), "claude-code" | "codex") {
        // Kept opt-in until the gateway's harness milestone replaces this adapter.
        return xflow_providers::build_harness_chat(config);
    }
    let mut ai = config.ai.clone();
    if !config.ask.provider.is_empty() && config.ask.provider != ai.provider {
        ai.provider.clone_from(&config.ask.provider);
        ai.model = None;
        ai.endpoint = None;
    }
    if !config.ask.model.is_empty() {
        ai.model = Some(config.ask.model.clone());
    }
    let reuse_cleanup = ai.provider.is_empty()
        && (config.cleanup.provider.is_some() || config.cleanup.endpoint.is_some());
    if ai.provider.is_empty() && !reuse_cleanup {
        ai.provider = "groq".into();
    }
    // One known destination per spoken question; no silent fallback to another provider.
    ai.fallback.clear();
    let (selected, gateway) =
        llm::build_provider_with_cleanup(&ai, &config.cleanup, config.privacy.offline)?;
    if selected.provider != "custom" && !config.ai.consented.contains(&selected.provider) {
        bail!(
            "allow {} first with xflow ask in a terminal",
            selected.provider
        );
    }
    let model = selected.model.unwrap_or_else(|| {
        xflow_providers::find_provider(xflow_providers::ProviderKind::Chat, &selected.provider)
            .map_or_else(String::new, |provider| provider.default_model.to_owned())
    });
    Ok(Arc::new(VoiceChat {
        gateway,
        provider: selected.provider,
        model,
    }))
}

#[async_trait]
impl ChatProvider for VoiceChat {
    fn name(&self) -> &str {
        &self.provider
    }
    fn model(&self) -> &str {
        &self.model
    }
    async fn warm(&self) -> Result<()> {
        self.gateway.warm().await.map_err(Into::into)
    }
    async fn stream(&self, question: &str, tx: mpsc::Sender<String>) -> Result<()> {
        let (events, mut rx) = mpsc::channel(16);
        let turn = async move {
            // Voice turns have no transcript sources or tools.
            let request = llm::ChatRequest {
                model: self.model.clone(),
                system: "Answer the user's question briefly. No transcript history or tools are available.".into(),
                messages: vec![llm::Message::User(question.to_owned())],
                tools: vec![],
                max_output_tokens: 512,
            };
            self.gateway.stream(&request, &events).await?;
            Ok::<_, anyhow::Error>(())
        };
        let receive = async {
            while let Some(event) = rx.recv().await {
                if let Event::Text(text) = event {
                    tx.send(text).await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        // Closing the text receiver cancels the whole turn, including the HTTP stream.
        tokio::try_join!(turn, receive)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn cleanup_gateway_sends_only_the_current_question_without_cleanup_instructions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut bytes = [0u8; 4096];
            let body_at = loop {
                let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&bytes[..n]);
                if let Some(at) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let headers = std::str::from_utf8(&request[..body_at]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap();
            while request.len() < body_at + length {
                let n = socket.read(&mut bytes).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&bytes[..n]);
            }
            let body: serde_json::Value = serde_json::from_slice(&request[body_at..]).unwrap();
            let answer = "data: {\"choices\":[{\"delta\":{\"content\":\"Brief answer.\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).as_bytes()).await.unwrap();
            body
        });
        let mut config = Config::default();
        config.cleanup.endpoint = Some(url);
        config.cleanup.model = Some("voice-model".into());
        config.cleanup.prompt = Some("CLEANUP ONLY INSTRUCTIONS".into());
        let voice = build(&config).unwrap();
        assert_eq!(voice.model(), "voice-model");
        let (tx, mut rx) = mpsc::channel(8);
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            voice.stream("Why use idempotency keys?", tx),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(rx.recv().await.as_deref(), Some("Brief answer."));
        let body = server.await.unwrap();
        assert_eq!(body["model"], "voice-model");
        assert_eq!(body["messages"][1]["content"], "Why use idempotency keys?");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            body["messages"][0]["content"],
            "Answer the user's question briefly. No transcript history or tools are available."
        );
        assert_eq!(body["max_tokens"], 512);
        assert!(body.get("tools").is_none());
        assert!(!body.to_string().contains("CLEANUP ONLY INSTRUCTIONS"));
    }

    #[test]
    fn explicit_hosted_destination_requires_saved_consent() {
        let mut config = Config::default();
        config.ai.provider = "groq".into();
        assert!(build(&config)
            .err()
            .unwrap()
            .to_string()
            .contains("allow groq"));
    }
}
