//! The one catalog of user-facing failures: a short title for the pill, and a
//! sentence with the next step for the TUI and CLI. Providers classify, clients present.
use serde::{Deserialize, Serialize};

/// What went wrong, as the user can act on it. On the wire as `{"kind":...}`;
/// a kind from a newer daemon reads as `Unknown` and falls back to the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Problem {
    KeyMissing,
    /// HTTP 401.
    KeyRejected,
    /// HTTP 403.
    Forbidden,
    /// HTTP 429, with the provider's `Retry-After` when it sent one.
    RateLimited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_s: Option<u64>,
    },
    QuotaExhausted,
    /// HTTP 5xx.
    ProviderDown,
    Timeout,
    /// The provider's name did not resolve (DNS): usually no internet.
    Offline,
    /// Connection refused or dropped.
    Unreachable,
    Microphone,
    PasteFailed,
    #[serde(other)]
    Unknown,
}

impl Problem {
    /// A few words, short enough for the pill.
    pub fn title(self) -> &'static str {
        match self {
            Self::KeyMissing => "No API key",
            Self::KeyRejected => "Key rejected",
            Self::Forbidden => "Access denied",
            Self::RateLimited { .. } => "Rate limited",
            Self::QuotaExhausted => "Out of credit",
            Self::ProviderDown => "Service is down",
            Self::Timeout => "Service too slow",
            Self::Offline => "You're offline",
            Self::Unreachable => "Can't reach service",
            Self::Microphone => "Microphone problem",
            Self::PasteFailed => "Couldn't paste",
            Self::Unknown => "Something went wrong",
        }
    }

    /// Cause and fix in one or two sentences. `name` is the service as shown to
    /// people ("Groq"), `id` as the CLI takes it ("groq"). None for `Unknown`.
    pub fn explain(self, name: &str, id: &str) -> Option<String> {
        let key = format!("Paste it in Settings, or run: xflow key set {id}");
        let other = "or choose another service in Settings.";
        Some(match self {
            Self::KeyMissing => format!("{name} needs an API key. {key}"),
            Self::KeyRejected => format!("{name} rejected the API key. {key}"),
            Self::Forbidden => format!(
                "{name} refused this key access. Check the key's permissions on {name}'s website, {other}"
            ),
            Self::RateLimited {
                retry_after_s: Some(s),
            } => format!(
                "{name} is rate-limiting you. Wait {} s and try again, {other}",
                s.max(1)
            ),
            Self::RateLimited { retry_after_s: None } => {
                format!("{name} is rate-limiting you. Wait a minute and try again, {other}")
            }
            Self::QuotaExhausted => format!(
                "Your {name} account is out of credit or quota. Add some on {name}'s website, {other}"
            ),
            Self::ProviderDown => {
                format!("{name} is having problems. Try again in a few minutes, {other}")
            }
            Self::Timeout => format!(
                "{name} took too long to answer. Check your internet connection and try again."
            ),
            Self::Offline => format!(
                "Can't look up {name}'s address, so you may be offline. Check your internet connection and try again."
            ),
            Self::Unreachable => format!(
                "Can't connect to {name}. Check your internet connection, VPN or firewall, and try again."
            ),
            Self::Microphone => "xflow can't record from your microphone. Check it's plugged in, choose it in Settings (xflow devices lists them), and allow microphone access in your system settings.".into(),
            Self::PasteFailed => "xflow couldn't paste here. Your text is saved: run xflow copy, or copy it from History, and paste it yourself.".into(),
            Self::Unknown => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind; the exhaustive matches above keep new kinds from being missed.
    const ALL: [Problem; 13] = [
        Problem::KeyMissing,
        Problem::KeyRejected,
        Problem::Forbidden,
        Problem::RateLimited {
            retry_after_s: Some(7),
        },
        Problem::RateLimited {
            retry_after_s: None,
        },
        Problem::QuotaExhausted,
        Problem::ProviderDown,
        Problem::Timeout,
        Problem::Offline,
        Problem::Unreachable,
        Problem::Microphone,
        Problem::PasteFailed,
        Problem::Unknown,
    ];

    #[test]
    fn every_kind_has_a_title_and_a_next_step() {
        for problem in ALL {
            assert!(!problem.title().is_empty() && problem.title().len() <= 24);
            let Some(text) = problem.explain("Groq", "groq") else {
                assert_eq!(problem, Problem::Unknown);
                continue;
            };
            assert!(text.ends_with('.') || text.ends_with("groq"), "{text}");
            assert!(
                ["Settings", "xflow ", "try again"]
                    .iter()
                    .any(|step| text.contains(step)),
                "no action: {text}"
            );
        }
        let wait = Problem::RateLimited {
            retry_after_s: Some(7),
        };
        assert!(wait.explain("Groq", "groq").unwrap().contains("Wait 7 s"));
        assert!(Problem::KeyRejected
            .explain("Groq", "groq")
            .unwrap()
            .contains("xflow key set groq"));
    }

    #[test]
    fn wire_shape_is_tagged_and_forward_compatible() {
        let json = serde_json::to_string(&Problem::RateLimited {
            retry_after_s: Some(7),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"rate_limited","retry_after_s":7}"#);
        assert_eq!(
            serde_json::from_str::<Problem>(r#"{"kind":"rate_limited"}"#).unwrap(),
            Problem::RateLimited {
                retry_after_s: None
            }
        );
        assert_eq!(
            serde_json::from_str::<Problem>(r#"{"kind":"from_the_future"}"#).unwrap(),
            Problem::Unknown
        );
    }
}
