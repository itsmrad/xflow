//! Static discovery metadata, not an allowlist: configured model ids may be newer.
#[derive(Clone, Copy, Debug)]
pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Stt,
    Cleanup,
}
#[derive(Clone, Copy, Debug)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ProviderKind,
    pub endpoint: &'static str,
    pub default_model: &'static str,
    pub models: &'static [ModelInfo],
    pub env_var: Option<&'static str>,
    pub key_url: &'static str,
    pub docs_url: &'static str,
    pub language: bool,
    pub vocabulary: bool,
    pub local: bool,
}
macro_rules! row {
    ($kind:ident, $id:literal, $name:literal, $endpoint:literal, $env:expr, $key:literal, $docs:literal, $hints:literal, $local:literal, [$(($model:literal, $label:literal, $note:literal)),+ $(,)?]) => {
        ProviderInfo { id: $id, name: $name, kind: ProviderKind::$kind, endpoint: $endpoint,
            default_model: row!(@first $($model),+), models: &[$(ModelInfo {id:$model, label:$label, note:$note}),+],
            env_var:$env, key_url:$key, docs_url:$docs, language: matches!(ProviderKind::$kind, ProviderKind::Stt), vocabulary:$hints, local:$local }
    };
    (@first $first:literal $(,$rest:literal)*) => { $first };
}
static STT: &[ProviderInfo] = &[
    row!(Stt,"groq","Groq","https://api.groq.com/openai/v1/audio/transcriptions",Some("GROQ_API_KEY"),"https://console.groq.com/keys","https://console.groq.com/docs/speech-to-text",true,false,[
        ("whisper-large-v3-turbo","Whisper Large v3 Turbo","Fast multilingual dictation"),
        ("whisper-large-v3","Whisper Large v3","Higher accuracy")]),
    row!(Stt,"openai","OpenAI","https://api.openai.com/v1/audio/transcriptions",Some("OPENAI_API_KEY"),"https://platform.openai.com/api-keys","https://platform.openai.com/docs/guides/speech-to-text",true,false,[
        ("gpt-transcribe","GPT Transcribe","Current file transcription; language and keyword arrays"),
        ("gpt-4o-mini-transcribe","GPT-4o Mini Transcribe","Legacy; scheduled retirement February 2027"),
        ("gpt-4o-transcribe","GPT-4o Transcribe","Legacy; scheduled retirement February 2027"),
        ("whisper-1","Whisper 1","Legacy; scheduled retirement February 2027")]),
    row!(Stt,"deepgram","Deepgram","https://api.deepgram.com/v1/listen",Some("DEEPGRAM_API_KEY"),"https://console.deepgram.com/","https://developers.deepgram.com/docs/pre-recorded-audio",true,false,[
        ("nova-3","Nova 3","Multilingual batch transcription"),
        ("nova-2","Nova 2","Legacy batch model; keyword hints")]),
    row!(Stt,"assemblyai","AssemblyAI","https://api.assemblyai.com/v2/transcript",Some("ASSEMBLYAI_API_KEY"),"https://www.assemblyai.com/dashboard/","https://www.assemblyai.com/docs/api-reference/transcripts/submit",true,false,[
        ("universal-3-5-pro","Universal 3.5 Pro","Async, auto language detection; sync protocol available"),
        ("universal-2","Universal 2","Broad language coverage; async")]),
    row!(Stt,"elevenlabs","ElevenLabs","https://api.elevenlabs.io/v1/speech-to-text",Some("ELEVENLABS_API_KEY"),"https://elevenlabs.io/app/settings/api-keys","https://elevenlabs.io/docs/api-reference/speech-to-text/convert",true,false,[
        ("scribe_v2","Scribe v2","Multilingual file transcription; keyterms cost extra")]),
    row!(Stt,"gemini","Google Gemini","https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-transcribe:generateContent",Some("GEMINI_API_KEY"),"https://aistudio.google.com/apikey","https://ai.google.dev/gemini-api/docs/generate-content/transcribe",true,false,[
        ("gemini-3.5-transcribe","Gemini 3.5 Transcribe","Dedicated verbatim ASR; inline audio requires account verification"),
        ("gemini-3.5-flash-lite","Gemini 3.5 Flash Lite","Prompted transcription; may hallucinate on silence")]),
    row!(Stt,"mistral","Mistral","https://api.mistral.ai/v1/audio/transcriptions",Some("MISTRAL_API_KEY"),"https://console.mistral.ai/api-keys/","https://docs.mistral.ai/studio/audio/speech_to_text/offline_transcription",true,false,[
        ("voxtral-mini-latest","Voxtral Mini Transcribe","Fast current transcription alias"),
        ("voxtral-mini-2602","Voxtral Mini Transcribe 2","Pinned February 2026 release")]),
    row!(Stt,"openrouter","OpenRouter","https://openrouter.ai/api/v1/audio/transcriptions",Some("OPENROUTER_API_KEY"),"https://openrouter.ai/settings/keys","https://openrouter.ai/docs/api/api-reference/stt/create-transcription",false,false,[
        ("openai/whisper-large-v3","Whisper Large v3","Multilingual routed transcription"),
        ("openai/whisper-large-v3-turbo","Whisper Large v3 Turbo","Fast routed transcription"),
        ("openai/gpt-transcribe","GPT Transcribe","Current OpenAI transcription")]),
    row!(Stt,"together","Together AI","https://api.together.ai/v1/audio/transcriptions",Some("TOGETHER_API_KEY"),"https://api.together.ai/settings/api-keys","https://docs.together.ai/docs/speech-to-text",true,false,[
        ("openai/whisper-large-v3","Whisper Large v3","Multilingual transcription"),
        ("nvidia/parakeet-tdt-0.6b-v3","Parakeet TDT v3","No vocabulary prompts")]),
    row!(Stt,"deepinfra","DeepInfra","https://api.deepinfra.com/v1/audio/transcriptions",Some("DEEPINFRA_API_KEY"),"https://deepinfra.com/dash/api_keys","https://docs.deepinfra.com/apis/speech",true,false,[
        ("openai/whisper-large-v3-turbo","Whisper Large v3 Turbo","Fast OpenAI-compatible transcription"),
        ("openai/whisper-large-v3","Whisper Large v3","Higher accuracy")]),
    row!(Stt,"custom","Custom","",None,"","https://platform.openai.com/docs/api-reference/audio",true,false,[
        ("","Server-specific model","Set endpoint and model; select stt.protocol explicitly")]),
    row!(Stt,"local","Local server","http://127.0.0.1:8000/v1/audio/transcriptions",None,"","https://speaches.ai/",true,true,[
        ("Systran/faster-whisper-small","Whisper Small","Preinstall model on your server; XFlow does not download it"),
        ("Systran/faster-whisper-large-v3","Whisper Large v3","Higher accuracy, more memory")]),
];
static CLEANUP: &[ProviderInfo] = &[
    row!(
        Cleanup,
        "openai",
        "OpenAI",
        "https://api.openai.com/v1/chat/completions",
        Some("OPENAI_API_KEY"),
        "https://platform.openai.com/api-keys",
        "https://platform.openai.com/docs/models",
        true,
        false,
        [
            ("gpt-4.1-nano", "GPT-4.1 Nano", "Fast editing"),
            ("gpt-4.1-mini", "GPT-4.1 Mini", "Higher quality editing")
        ]
    ),
    row!(
        Cleanup,
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1/chat/completions",
        Some("GROQ_API_KEY"),
        "https://console.groq.com/keys",
        "https://console.groq.com/docs/models",
        true,
        false,
        [
            (
                "llama-3.1-8b-instant",
                "Llama 3.1 8B Instant",
                "Fast non-reasoning editor"
            ),
            ("llama-3.3-70b-versatile", "Llama 3.3 70B", "Higher quality")
        ]
    ),
    row!(
        Cleanup,
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1/chat/completions",
        Some("OPENROUTER_API_KEY"),
        "https://openrouter.ai/settings/keys",
        "https://openrouter.ai/docs/api-reference/overview",
        true,
        false,
        [(
            "google/gemini-3.5-flash-lite",
            "Gemini 3.5 Flash Lite",
            "Fast general editing"
        )]
    ),
    row!(
        Cleanup,
        "together",
        "Together AI",
        "https://api.together.ai/v1/chat/completions",
        Some("TOGETHER_API_KEY"),
        "https://api.together.ai/settings/api-keys",
        "https://docs.together.ai/docs/serverless-models",
        true,
        false,
        [(
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "Llama 3.3 70B Turbo",
            "Instruction-following editor"
        )]
    ),
    row!(
        Cleanup,
        "fireworks",
        "Fireworks AI",
        "https://api.fireworks.ai/inference/v1/chat/completions",
        Some("FIREWORKS_API_KEY"),
        "https://app.fireworks.ai/settings/users/api-keys",
        "https://docs.fireworks.ai/api-reference/post-chatcompletions",
        true,
        false,
        [(
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
            "Llama 3.3 70B",
            "Non-reasoning editor; verify account availability"
        )]
    ),
    row!(
        Cleanup,
        "mistral",
        "Mistral",
        "https://api.mistral.ai/v1/chat/completions",
        Some("MISTRAL_API_KEY"),
        "https://console.mistral.ai/api-keys/",
        "https://docs.mistral.ai/api/endpoint/chat",
        true,
        false,
        [(
            "mistral-small-latest",
            "Mistral Small",
            "Current small editor alias"
        )]
    ),
    row!(
        Cleanup,
        "deepinfra",
        "DeepInfra",
        "https://api.deepinfra.com/v1/openai/chat/completions",
        Some("DEEPINFRA_API_KEY"),
        "https://deepinfra.com/dash/api_keys",
        "https://docs.deepinfra.com/chat/overview",
        true,
        false,
        [(
            "meta-llama/Llama-3.3-70B-Instruct",
            "Llama 3.3 70B",
            "Non-reasoning editor"
        )]
    ),
    row!(
        Cleanup,
        "gemini",
        "Google Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        Some("GEMINI_API_KEY"),
        "https://aistudio.google.com/apikey",
        "https://ai.google.dev/gemini-api/docs/openai",
        true,
        false,
        [(
            "gemini-3.5-flash-lite",
            "Gemini 3.5 Flash Lite",
            "Low latency editing"
        )]
    ),
    row!(
        Cleanup,
        "ollama",
        "Ollama",
        "http://127.0.0.1:11434/v1/chat/completions",
        None,
        "",
        "https://docs.ollama.com/api/openai-compatibility",
        true,
        true,
        [(
            "llama3.2:3b",
            "Llama 3.2 3B",
            "Install model in Ollama first"
        )]
    ),
    row!(
        Cleanup,
        "custom",
        "Custom",
        "",
        None,
        "",
        "https://platform.openai.com/docs/api-reference/chat",
        true,
        false,
        [("", "Server-specific model", "Set endpoint and model")]
    ),
];
pub fn stt_providers() -> &'static [ProviderInfo] {
    STT
}
pub fn cleanup_providers() -> &'static [ProviderInfo] {
    CLEANUP
}
pub fn find_provider(kind: ProviderKind, id: &str) -> Option<&'static ProviderInfo> {
    let rows = match kind {
        ProviderKind::Stt => STT,
        ProviderKind::Cleanup => CLEANUP,
    };
    rows.iter().find(|p| p.id == id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_invariants() {
        for rows in [STT, CLEANUP] {
            let mut ids = std::collections::HashSet::new();
            for p in rows {
                assert!(ids.insert(p.id));
                assert!(p.models.iter().any(|m| m.id == p.default_model));
                if let Some(env) = p.env_var {
                    assert!(env.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'));
                }
            }
        }
        assert_eq!(
            STT.iter().filter(|p| !p.local && p.id != "custom").count(),
            10
        );
    }
}
