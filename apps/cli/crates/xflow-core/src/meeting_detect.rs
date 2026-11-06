//! Meeting detection decisions from timestamped snapshots: which meeting apps
//! hold microphone input, and which meeting windows they show. Pure: no I/O and
//! no clock; the platform observer supplies both. Adding an app is one row in
//! [`ADAPTERS`]. Unknown apps never produce a decision.
//!
//! Titles and URLs are matched in memory only. A [`MeetingId`] holds hashes,
//! never text, so logging a decision cannot leak them.

/// Input held this long, with the same meeting identity, starts a session.
pub const START_AFTER_MS: u64 = 2_000;
/// How long input, a meeting window or an app must be gone before it counts.
pub const GONE_AFTER_MS: u64 = 8_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The app's own process (or a helper inside it) holds the microphone.
    Native,
    /// A browser holds the microphone and one of its windows shows the meeting.
    Browser,
}

#[derive(Debug)]
pub struct Adapter {
    pub name: &'static str,
    pub kind: Kind,
    /// Parent-app bundle ids: the outermost `.app`, never a helper.
    pub bundles: &'static [&'static str],
    /// Browser rows: hosts matched exactly or as a subdomain, `https` only.
    pub https_hosts: &'static [&'static str],
    /// Case-insensitive title patterns; `@` matches one ASCII letter. Native
    /// rows: when windows are readable, one must match (a lobby or settings
    /// window does not). Browser rows: used only when the window has no URL.
    pub title: &'static [&'static str],
    /// A window whose title contains one of these never matches.
    pub reject_title: &'static [&'static str],
    /// Checked against a real install and a real call.
    pub verified: bool,
}

/// Parent-app bundle ids of browsers. Chrome, Safari and Helium were read from
/// their installed Info.plist; the rest follow their vendors' published ids.
pub const BROWSERS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.dev",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "com.microsoft.edgemac",
    "com.microsoft.edgemac.Beta",
    "com.microsoft.edgemac.Dev",
    "com.microsoft.edgemac.Canary",
    "com.brave.Browser",
    "com.brave.Browser.beta",
    "com.brave.Browser.nightly",
    "company.thebrowser.Browser",
    "company.thebrowser.dia",
    "net.imput.helium",
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "org.mozilla.nightly",
];

/// Meeting apps. None is verified on a real call yet: Zoom, Teams and Chime
/// were not installed on the test Mac, and browser titles were read only from
/// local test pages. AWS ended Amazon Chime support on 2026-02-20.
pub const ADAPTERS: &[Adapter] = &[
    Adapter {
        name: "Zoom",
        kind: Kind::Native,
        bundles: &["us.zoom.xos"],
        https_hosts: &[],
        title: &["Zoom Meeting", "Zoom Webinar"],
        reject_title: &[],
        verified: false,
    },
    Adapter {
        name: "Microsoft Teams",
        kind: Kind::Native,
        bundles: &["com.microsoft.teams2", "com.microsoft.teams"],
        https_hosts: &[],
        title: &[],
        reject_title: &[],
        verified: false,
    },
    Adapter {
        name: "Amazon Chime",
        kind: Kind::Native,
        bundles: &["com.amazon.Amazon-Chime"],
        https_hosts: &[],
        title: &[],
        reject_title: &[],
        verified: false,
    },
    Adapter {
        name: "Google Meet",
        kind: Kind::Browser,
        bundles: BROWSERS,
        https_hosts: &["meet.google.com"],
        title: &["Meet - @@@-@@@@-@@@"],
        reject_title: &[" - Google Search"],
        verified: false,
    },
    Adapter {
        name: "Microsoft Teams",
        kind: Kind::Browser,
        bundles: BROWSERS,
        https_hosts: &["teams.microsoft.com", "teams.live.com"],
        title: &[],
        reject_title: &[],
        verified: false,
    },
    Adapter {
        name: "Zoom",
        kind: Kind::Browser,
        bundles: BROWSERS,
        https_hosts: &["zoom.us"],
        title: &["Zoom Meeting"],
        reject_title: &[" - Google Search"],
        verified: false,
    },
    Adapter {
        name: "Amazon Chime",
        kind: Kind::Browser,
        bundles: BROWSERS,
        https_hosts: &["app.chime.aws", "chime.aws"],
        title: &[],
        reject_title: &[],
        verified: false,
    },
];

/// One window of a watched app, read by Accessibility.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub title: String,
    pub url: Option<String>,
    /// Some(true): a Leave control; Some(false): an explicit post-call screen.
    /// None: unreadable, hidden, pre-join or otherwise ambiguous.
    pub in_call: Option<bool>,
}

/// One watched app in a snapshot. An app missing from the snapshot is not running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppSeen {
    pub bundle: String,
    /// Any of its processes holds microphone input.
    pub input: bool,
    /// None when windows were not read or could not be read.
    pub windows: Option<Vec<Window>>,
}
impl AppSeen {
    pub fn new(bundle: &str, input: bool, windows: Option<Vec<Window>>) -> Self {
        Self {
            bundle: bundle.to_owned(),
            input,
            windows,
        }
    }
}

/// A meeting's identity: adapter, app and (for browsers) the meeting code. Hashes only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeetingId {
    adapter: usize,
    app: u64,
    code: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Ask "record this meeting?".
    Start,
    /// Input stopped but the meeting may still be there: ask, never stop silently.
    MaybeEnded,
    /// The meeting is gone: input off and its window gone, or the app quit.
    End,
}
impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::MaybeEnded => "maybe ended",
            Self::End => "end",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub at_ms: u64,
    /// The adapter's name, never a title or URL.
    pub app: &'static str,
    pub decision: Decision,
    pub meeting: MeetingId,
    /// Parent bundle to pass to `xflow meeting record --app`.
    pub bundle: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visible {
    Yes,
    No,
    Unknown,
}

#[derive(Debug)]
struct Session {
    id: MeetingId,
    bundle: String,
    declined: bool,
    quiet_since: Option<u64>,
    gone_since: Option<u64>,
    absent_since: Option<u64>,
    /// Not present (declined sessions expire on this).
    missing_since: Option<u64>,
    asked: bool,
}

/// The state machine. Feed it a snapshot at least once a second while
/// [`Detector::wants_poll`] says so.
#[derive(Debug, Default)]
pub struct Detector {
    pending: Vec<(MeetingId, u64)>,
    sessions: Vec<Session>,
}

impl Detector {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bundle belongs to a meeting app or browser: the observer reports it.
    pub fn watches(bundle: &str) -> bool {
        ADAPTERS.iter().any(|a| a.bundles.contains(&bundle))
    }

    /// Whether the observer should read this app's windows now.
    pub fn wants_windows(&self, bundle: &str, input: bool) -> bool {
        Self::watches(bundle) && (input || self.sessions.iter().any(|s| s.bundle == bundle))
    }

    /// Poll once a second while the input device runs (another app may start
    /// without a device edge when xflow already holds it) or a meeting is
    /// pending or live; otherwise listeners alone wake the observer.
    pub fn wants_poll(&self, device_running: bool) -> bool {
        device_running || !self.pending.is_empty() || !self.sessions.is_empty()
    }

    /// The user said no: no more decisions for this meeting, and no new start
    /// until it has been gone for [`GONE_AFTER_MS`].
    pub fn decline(&mut self, meeting: MeetingId) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == meeting) {
            s.declined = true;
        }
    }

    pub fn update(&mut self, at_ms: u64, apps: &[AppSeen]) -> Vec<Event> {
        let mut events = Vec::new();
        let candidates = candidates(apps);

        // Live sessions: end, maybe ended, or a declined one expiring.
        let mut index = 0;
        while index < self.sessions.len() {
            let s = &mut self.sessions[index];
            let app = apps.iter().find(|a| a.bundle == s.bundle);
            let visible = app.map_or(Visible::No, |a| visibility(s.id, a));
            let input = app.is_some_and(|a| a.input);
            let present = app.is_some() && (input || visible == Visible::Yes);
            s.missing_since = if present {
                None
            } else {
                s.missing_since.or(Some(at_ms))
            };
            s.absent_since = if app.is_some() {
                None
            } else {
                s.absent_since.or(Some(at_ms))
            };
            s.quiet_since = if input {
                s.asked = false;
                None
            } else {
                s.quiet_since.or(Some(at_ms))
            };
            s.gone_since = if visible == Visible::No {
                s.gone_since.or(Some(at_ms))
            } else {
                None
            };
            let held = |since: Option<u64>| {
                since.is_some_and(|t| at_ms.saturating_sub(t) >= GONE_AFTER_MS)
            };
            let browser = ADAPTERS[s.id.adapter].kind == Kind::Browser;
            // A background tab looks exactly like a closed one (window titles show
            // the active tab only), so a browser never ends on "window gone" alone.
            // Only Meet currently supplies a call code; a Teams landing URL
            // cannot distinguish an ended tab from another background call.
            let identified = ADAPTERS[s.id.adapter].title.iter().any(|p| p.contains('@'));
            let ended = identified
                && app.is_some_and(|a| {
                    a.windows.as_ref().is_some_and(|ws| {
                        ws.iter().any(|w| {
                            w.in_call == Some(false) && browser_id(&a.bundle, w) == Some(s.id)
                        }) && ws
                            .iter()
                            .filter(|w| browser_id(&a.bundle, w) == Some(s.id))
                            .all(|w| w.in_call == Some(false))
                    })
                });
            let gone = held(s.gone_since) && !browser && held(s.quiet_since);
            let ended = ended && held(s.quiet_since);
            let id = s.id;
            if s.declined {
                if held(s.missing_since) {
                    self.sessions.remove(index);
                    continue;
                }
            } else if held(s.absent_since) || gone || ended {
                events.push(event(at_ms, id, Decision::End));
                self.sessions.remove(index);
                continue;
            } else if held(s.quiet_since) && !s.asked {
                s.asked = true;
                events.push(event(at_ms, id, Decision::MaybeEnded));
            }
            index += 1;
        }

        // Starts: the same identity held for START_AFTER_MS; a drop cancels.
        let live = |id: &MeetingId, sessions: &[Session]| sessions.iter().any(|s| s.id == *id);
        self.pending
            .retain(|(id, _)| candidates.iter().any(|c| c.0 == *id) && !live(id, &self.sessions));
        for (id, bundle) in candidates {
            if live(&id, &self.sessions) {
                continue;
            }
            let since = match self.pending.iter().find(|p| p.0 == id) {
                Some(&(_, since)) => since,
                None => {
                    self.pending.push((id, at_ms));
                    at_ms
                }
            };
            if at_ms.saturating_sub(since) >= START_AFTER_MS {
                self.pending.retain(|p| p.0 != id);
                self.sessions.push(Session {
                    id,
                    bundle,
                    declined: false,
                    quiet_since: None,
                    gone_since: None,
                    absent_since: None,
                    missing_since: None,
                    asked: false,
                });
                events.push(event(at_ms, id, Decision::Start));
            }
        }
        events
    }
}

fn event(at_ms: u64, meeting: MeetingId, decision: Decision) -> Event {
    Event {
        at_ms,
        app: ADAPTERS[meeting.adapter].name,
        decision,
        meeting,
        bundle: ADAPTERS[meeting.adapter]
            .bundles
            .iter()
            .copied()
            .find(|b| crate::stable_hash(b.as_bytes()) == meeting.app)
            .unwrap_or(""),
    }
}

/// Meetings that may start now, with their app's bundle.
fn candidates(apps: &[AppSeen]) -> Vec<(MeetingId, String)> {
    let mut out = Vec::new();
    for app in apps.iter().filter(|a| a.input) {
        for (index, adapter) in ADAPTERS.iter().enumerate() {
            if adapter.kind != Kind::Native || !adapter.bundles.contains(&app.bundle.as_str()) {
                continue;
            }
            let id = native_id(index, &app.bundle);
            if app
                .windows
                .as_ref()
                .is_some_and(|ws| ws.iter().any(|w| w.in_call == Some(true)))
                && visibility(id, app) == Visible::Yes
            {
                out.push((id, app.bundle.clone()));
            }
        }
        // One input bit per browser: two meetings at once are ambiguous, stay silent.
        let mut found: Vec<MeetingId> = Vec::new();
        for window in app.windows.iter().flatten() {
            if let Some(id) =
                browser_id(&app.bundle, window).filter(|_| window.in_call == Some(true))
            {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
        if let [id] = found[..] {
            out.push((id, app.bundle.clone()));
        }
    }
    out
}

fn native_id(adapter: usize, bundle: &str) -> MeetingId {
    MeetingId {
        adapter,
        app: crate::stable_hash(bundle.as_bytes()),
        code: 0,
    }
}

fn visibility(id: MeetingId, app: &AppSeen) -> Visible {
    let adapter = &ADAPTERS[id.adapter];
    // No windows at all (hidden, another Space, unreadable) tells nothing.
    let Some(windows) = app.windows.as_ref().filter(|w| !w.is_empty()) else {
        return Visible::Unknown;
    };
    let seen = match adapter.kind {
        Kind::Native if adapter.title.is_empty() => {
            if windows.iter().any(|w| w.in_call == Some(true)) {
                return Visible::Yes;
            }
            if windows.iter().all(|w| w.in_call == Some(false)) {
                return Visible::No;
            }
            return Visible::Unknown;
        }
        Kind::Native => windows.iter().any(|w| {
            w.in_call != Some(false)
                && !rejected(adapter, &w.title)
                && (w.in_call == Some(true)
                    || adapter.title.iter().any(|p| find(p, &w.title).is_some()))
        }),
        Kind::Browser => windows
            .iter()
            .any(|w| w.in_call != Some(false) && browser_id(&app.bundle, w) == Some(id)),
    };
    if seen {
        Visible::Yes
    } else {
        Visible::No
    }
}

/// The browser meeting a window shows. With a URL, the host decides; without
/// one, the title does.
fn browser_id(bundle: &str, window: &Window) -> Option<MeetingId> {
    ADAPTERS.iter().enumerate().find_map(|(index, adapter)| {
        if adapter.kind != Kind::Browser
            || !adapter.bundles.contains(&bundle)
            || rejected(adapter, &window.title)
        {
            return None;
        }
        let has_code = adapter.title.iter().any(|p| p.contains('@'));
        let code =
            match &window.url {
                Some(url) => {
                    let (host, path) = https_host(url)?;
                    if !adapter.https_hosts.iter().any(|h| {
                        host == *h || host.strip_suffix(h).is_some_and(|r| r.ends_with('.'))
                    }) {
                        return None;
                    }
                    if has_code {
                        path.split('/')
                            .find(|p| !p.is_empty())
                            .unwrap_or("")
                            .to_ascii_lowercase()
                    } else {
                        String::new()
                    }
                }
                None => {
                    let (pattern, at) = adapter
                        .title
                        .iter()
                        .find_map(|p| find(p, &window.title).map(|at| (*p, at)))?;
                    if has_code {
                        // The code is what follows the pattern's literal prefix.
                        let prefix = pattern.find('@').unwrap_or(0);
                        window.title[at + prefix..at + pattern.len()].to_ascii_lowercase()
                    } else {
                        String::new()
                    }
                }
            };
        Some(MeetingId {
            adapter: index,
            app: crate::stable_hash(bundle.as_bytes()),
            code: crate::stable_hash(code.as_bytes()),
        })
    })
}

fn rejected(adapter: &Adapter, title: &str) -> bool {
    adapter
        .reject_title
        .iter()
        .any(|r| find(r, title).is_some())
}

/// Byte offset of the first case-insensitive match of `pattern` in `text`,
/// where `@` in the pattern matches one ASCII letter. Patterns are ASCII.
fn find(pattern: &str, text: &str) -> Option<usize> {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    if p.is_empty() || p.len() > t.len() {
        return None;
    }
    (0..=t.len() - p.len()).find(|&start| {
        text.is_char_boundary(start)
            && p.iter().zip(&t[start..]).all(|(&pc, &tc)| {
                if pc == b'@' {
                    tc.is_ascii_alphabetic()
                } else {
                    pc.eq_ignore_ascii_case(&tc)
                }
            })
    })
}

/// Lowercased host and the path of an `https` URL. Userinfo, port and a
/// trailing dot are removed, so `https://meet.google.com@evil.example/` is
/// `evil.example`.
fn https_host(url: &str) -> Option<(String, &str)> {
    let rest = url
        .get(..8)?
        .eq_ignore_ascii_case("https://")
        .then(|| &url[8..])?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?.trim_end_matches('.');
    (!host.is_empty()).then(|| (host.to_ascii_lowercase(), path))
}

/// The outermost `.app` in an executable's path: a helper's parent app.
pub fn outermost_app(path: &str) -> Option<&str> {
    let mut end = 0;
    for part in path.split('/') {
        end += part.len();
        if part.len() > 4 && part.ends_with(".app") {
            return Some(&path[..end]);
        }
        end += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZOOM: &str = "us.zoom.xos";
    const TEAMS: &str = "com.microsoft.teams2";
    const CHROME: &str = "com.google.Chrome";

    fn win(title: &str) -> Window {
        Window {
            title: title.into(),
            url: None,
            in_call: Some(true),
        }
    }
    fn page(title: &str, url: &str) -> Window {
        Window {
            title: title.into(),
            url: Some(url.into()),
            in_call: Some(true),
        }
    }
    fn zoom(input: bool, meeting_window: bool) -> AppSeen {
        let title = if meeting_window {
            "Zoom Meeting"
        } else {
            "Zoom Workplace"
        };
        AppSeen::new(
            ZOOM,
            input,
            Some(vec![Window {
                in_call: meeting_window.then_some(true),
                ..win(title)
            }]),
        )
    }
    fn chrome(input: bool, windows: Vec<Window>) -> AppSeen {
        AppSeen::new(CHROME, input, Some(windows))
    }
    fn meet(code: &str) -> Window {
        page(
            &format!("Meet - {code}"),
            &format!("https://meet.google.com/{code}?authuser=0"),
        )
    }

    /// A labelled trace: (ms, apps) steps; returns "ms app decision" lines.
    fn run(detector: &mut Detector, trace: &[(u64, Vec<AppSeen>)]) -> Vec<String> {
        trace
            .iter()
            .flat_map(|(at, apps)| detector.update(*at, apps))
            .map(|e| format!("{} {} {}", e.at_ms, e.app, e.decision.as_str()))
            .collect()
    }
    /// The same apps every second from `from` to `to` inclusive.
    fn hold(from: u64, to: u64, apps: Vec<AppSeen>) -> Vec<(u64, Vec<AppSeen>)> {
        (from..=to)
            .step_by(1000)
            .map(|t| (t, apps.clone()))
            .collect()
    }

    #[test]
    fn another_browser_call_is_not_proof_the_background_call_ended() {
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![chrome(true, vec![meet("abc-defg-hij")])]);
        trace.extend(hold(
            4000,
            60_000,
            vec![chrome(true, vec![meet("xyz-wxyz-xyz")])],
        ));
        let events = run(&mut d, &trace);
        assert_eq!(events, ["2000 Google Meet start", "6000 Google Meet start"]);
    }

    #[test]
    fn browser_prejoin_and_unrelated_microphone_stay_silent() {
        for in_call in [None, Some(false)] {
            let mut w = meet("abc-defg-hij");
            w.in_call = in_call;
            let apps = vec![chrome(true, vec![w])];
            assert!(run(&mut Detector::new(), &hold(0, 20_000, apps)).is_empty());
        }
    }

    #[test]
    fn explicit_browser_end_stops_but_hidden_or_muted_call_does_not() {
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![chrome(true, vec![meet("abc-defg-hij")])]);
        trace.extend(hold(
            4000,
            20_000,
            vec![chrome(false, vec![meet("abc-defg-hij")])],
        ));
        let mut ended = meet("abc-defg-hij");
        ended.in_call = Some(false);
        trace.push((21_000, vec![chrome(false, vec![ended])]));
        assert_eq!(
            run(&mut d, &trace),
            [
                "2000 Google Meet start",
                "12000 Google Meet maybe ended",
                "21000 Google Meet end"
            ]
        );
    }

    #[test]
    fn an_ended_browser_window_never_overrides_a_live_window() {
        let mut d = Detector::new();
        let call = meet("abc-defg-hij");
        let mut ended = call.clone();
        ended.in_call = Some(false);
        let mut trace = hold(0, 3000, vec![chrome(true, vec![call.clone()])]);
        trace.extend(hold(4000, 20_000, vec![chrome(false, vec![call, ended])]));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Google Meet start", "12000 Google Meet maybe ended"]
        );
    }

    #[test]
    fn event_names_the_parent_bundle_for_the_recorder() {
        let mut d = Detector::new();
        d.update(0, &[chrome(true, vec![meet("abc-defg-hij")])]);
        assert_eq!(
            d.update(2000, &[chrome(true, vec![meet("abc-defg-hij")])])[0].bundle,
            CHROME
        );
    }

    #[test]
    fn starts_after_the_debounce() {
        let mut d = Detector::new();
        let mut trace = vec![(0, vec![zoom(true, true)]), (1500, vec![zoom(true, true)])];
        trace.push((2000, vec![zoom(true, true)]));
        trace.push((3000, vec![zoom(true, true)]));
        assert_eq!(run(&mut d, &trace), ["2000 Zoom start"]);
    }

    #[test]
    fn a_drop_before_the_debounce_cancels() {
        let mut d = Detector::new();
        let trace = [
            (0, vec![zoom(true, true)]),
            (1000, vec![zoom(true, true)]),
            (1900, vec![zoom(false, true)]),
            (2500, vec![zoom(true, true)]),
            (4000, vec![zoom(true, true)]),
            (4500, vec![zoom(true, true)]),
        ];
        // The restart at 2500 needs its own full 2 s.
        assert!(run(&mut d, &trace[..5]).is_empty());
        assert_eq!(run(&mut d, &trace[5..]), ["4500 Zoom start"]);
    }

    #[test]
    fn own_dictation_holding_the_mic_never_hides_a_meeting() {
        let mut d = Detector::new();
        // xflow holds the input, so the device bit stays 1 and no edge fires;
        // the poll must keep running and xflow itself is not a meeting.
        assert!(d.wants_poll(true));
        assert!(!d.wants_poll(false));
        let own = AppSeen::new("com.xflow.xflowd", true, None);
        assert!(!Detector::watches("com.xflow.xflowd"));
        assert!(run(&mut d, &hold(0, 5000, vec![own.clone()])).is_empty());
        let both = vec![own, zoom(true, true)];
        assert_eq!(run(&mut d, &hold(6000, 8000, both)), ["8000 Zoom start"]);
    }

    #[test]
    fn mic_preview_in_a_lobby_waits_for_the_meeting_window() {
        let mut d = Detector::new();
        let mut trace = hold(0, 10_000, vec![zoom(true, false)]);
        trace.extend(hold(11_000, 13_000, vec![zoom(true, true)]));
        assert_eq!(run(&mut d, &trace), ["13000 Zoom start"]);
    }

    #[test]
    fn native_without_window_information_stays_silent() {
        let mut d = Detector::new();
        let trace = hold(0, 2000, vec![AppSeen::new(ZOOM, true, None)]);
        assert!(run(&mut d, &trace).is_empty());
    }

    #[test]
    fn a_decline_is_suppressed_until_a_distinct_session() {
        let mut d = Detector::new();
        let events = d.update(0, &[zoom(true, true)]);
        assert!(events.is_empty());
        let start = d.update(2000, &[zoom(true, true)]);
        assert_eq!(start.len(), 1);
        d.decline(start[0].meeting);
        // Muting or a short gap inside the same call never asks again.
        let mut trace = hold(3000, 20_000, vec![zoom(true, true)]);
        trace.extend(hold(21_000, 25_000, vec![]));
        trace.extend(hold(26_000, 40_000, vec![zoom(true, true)]));
        assert!(run(&mut d, &trace).is_empty(), "declined call stays quiet");
        // Gone for 8 s: the next call is a new session.
        let mut trace = hold(41_000, 50_000, vec![]);
        trace.extend(hold(51_000, 53_000, vec![zoom(true, true)]));
        assert_eq!(run(&mut d, &trace), ["53000 Zoom start"]);
    }

    #[test]
    fn back_to_back_calls() {
        let mut d = Detector::new();
        let mut trace = hold(0, 5000, vec![zoom(true, true)]);
        // Leave: input off and the meeting window closes together.
        trace.extend(hold(6000, 14_000, vec![zoom(false, false)]));
        trace.extend(hold(15_000, 17_000, vec![zoom(true, true)]));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Zoom start", "14000 Zoom end", "17000 Zoom start"]
        );
        // Browser: one tab moves from one Meet code to another.
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![chrome(true, vec![meet("abc-defg-hij")])]);
        trace.extend(hold(
            4000,
            13_000,
            vec![chrome(true, vec![meet("xyz-wxyz-xyz")])],
        ));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Google Meet start", "6000 Google Meet start"]
        );
    }

    #[test]
    fn mute_with_the_window_present_never_ends_silently() {
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![zoom(true, true)]);
        trace.extend(hold(4000, 60_000, vec![zoom(false, true)]));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Zoom start", "12000 Zoom maybe ended"]
        );
        // Unmute clears it; a later mute asks again.
        let mut trace = hold(61_000, 62_000, vec![zoom(true, true)]);
        trace.extend(hold(63_000, 71_000, vec![zoom(false, true)]));
        assert_eq!(run(&mut d, &trace), ["71000 Zoom maybe ended"]);
        // The window then closes: ended, 8 s after it went.
        let trace = hold(72_000, 80_000, vec![zoom(false, false)]);
        assert_eq!(run(&mut d, &trace), ["80000 Zoom end"]);
    }

    #[test]
    fn no_window_information_asks_and_quitting_ends() {
        let mut d = Detector::new();
        let teams = |input| AppSeen::new(TEAMS, input, Some(vec![win("Chat | Microsoft Teams")]));
        let mut trace = hold(0, 3000, vec![teams(true)]);
        trace.extend(hold(4000, 30_000, vec![teams(false)]));
        assert_eq!(
            run(&mut d, &trace),
            [
                "2000 Microsoft Teams start",
                "12000 Microsoft Teams maybe ended"
            ]
        );
        // The app quits: ended after 8 s gone.
        assert_eq!(
            run(&mut d, &hold(31_000, 39_000, vec![])),
            ["39000 Microsoft Teams end"]
        );
    }

    #[test]
    fn a_stale_process_object_does_not_end_a_call() {
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![zoom(true, true)]);
        // One reconcile missed Zoom (its object went stale and was re-listed).
        trace.push((4000, vec![]));
        trace.extend(hold(5000, 30_000, vec![zoom(true, true)]));
        assert_eq!(run(&mut d, &trace), ["2000 Zoom start"]);
    }

    #[test]
    fn browser_hosts_and_titles_reject_lookalikes() {
        let mut d = Detector::new();
        let lookalikes = vec![
            page(
                "Meet - abc-defg-hij",
                "https://meet.google.com.evil.example/abc-defg-hij",
            ),
            page(
                "Meet - abc-defg-hij",
                "https://meet.google.com@evil.example/abc-defg-hij",
            ),
            page("Meet - abc-defg-hij", "http://meet.google.com/abc-defg-hij"),
            win("Google Meet - Google Search"),
            win("Meet - abc-defg-hij - Google Search"),
            page(
                "Meet - abc-defg-hij",
                "https://www.google.com/search?q=meet",
            ),
            win("Google Meet"),
            page("Zoom", "https://notzoom.us/j/1"),
        ];
        for window in lookalikes {
            let apps = vec![chrome(true, vec![window.clone()])];
            assert!(run(&mut d, &hold(0, 5000, apps)).is_empty(), "{window:?}");
        }
        let accepted = [
            win("Meet - abc-defg-hij"),
            page("anything", "https://MEET.google.com./abc-defg-hij"),
            page("Zoom", "https://app.zoom.us/wc/123/join"),
            page("Teams", "https://teams.microsoft.com/v2/"),
            page("Chime", "https://app.chime.aws/meetings/1"),
        ];
        for window in accepted {
            let mut d = Detector::new();
            let apps = vec![chrome(true, vec![window.clone()])];
            assert_eq!(run(&mut d, &hold(0, 2000, apps)).len(), 1, "{window:?}");
        }
    }

    #[test]
    fn browsers_need_window_information_and_unknown_apps_stay_silent() {
        let mut d = Detector::new();
        let apps = vec![
            AppSeen::new(CHROME, true, None),
            AppSeen::new("com.hnc.Discord", true, Some(vec![win("Zoom Meeting")])),
            AppSeen::new("com.apple.FaceTime", true, None),
        ];
        assert!(run(&mut d, &hold(0, 10_000, apps)).is_empty());
    }

    #[test]
    fn a_helper_maps_to_its_parent_app() {
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";
        assert_eq!(
            outermost_app(helper),
            Some("/Applications/Google Chrome.app")
        );
        assert_eq!(
            outermost_app("/Applications/zoom.us.app/Contents/MacOS/zoom.us"),
            Some("/Applications/zoom.us.app")
        );
        assert_eq!(outermost_app("/usr/local/bin/xflowd"), None);
        assert_eq!(outermost_app("/tmp/.app/x"), None);
    }

    #[test]
    fn media_only_tabs_never_start() {
        let mut d = Detector::new();
        // A Meet window is open but the browser only plays sound.
        let apps = vec![chrome(
            false,
            vec![meet("abc-defg-hij"), win("Music video")],
        )];
        assert!(run(&mut d, &hold(0, 10_000, apps)).is_empty());
    }

    #[test]
    fn two_browser_meetings_at_once_stay_silent_until_one_closes() {
        let mut d = Detector::new();
        let two = vec![chrome(
            true,
            vec![meet("abc-defg-hij"), meet("xyz-wxyz-xyz")],
        )];
        let mut trace = hold(0, 10_000, two);
        trace.extend(hold(
            11_000,
            13_000,
            vec![chrome(true, vec![meet("xyz-wxyz-xyz")])],
        ));
        assert_eq!(run(&mut d, &trace), ["13000 Google Meet start"]);
        // The URL and title forms of one meeting are the same meeting.
        let both = vec![chrome(
            true,
            vec![meet("abc-defg-hij"), win("Meet - abc-defg-hij")],
        )];
        assert_eq!(run(&mut Detector::new(), &hold(0, 2000, both)).len(), 1);
    }

    #[test]
    fn a_background_meeting_tab_with_input_off_only_asks() {
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![chrome(true, vec![meet("abc-defg-hij")])]);
        // The user switches tabs (the title is now another page) and the mic drops.
        // A closed tab looks the same, so this asks instead of stopping.
        trace.extend(hold(4000, 30_000, vec![chrome(false, vec![win("Inbox")])]));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Google Meet start", "12000 Google Meet maybe ended"]
        );
        // Quitting the browser ends it.
        assert_eq!(
            run(&mut d, &hold(31_000, 39_000, vec![])),
            ["39000 Google Meet end"]
        );
        // Unreadable windows: asks too.
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![chrome(true, vec![meet("abc-defg-hij")])]);
        trace.extend(hold(4000, 30_000, vec![AppSeen::new(CHROME, false, None)]));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Google Meet start", "12000 Google Meet maybe ended"]
        );
    }

    #[test]
    fn a_native_app_with_no_windows_listed_only_asks() {
        let mut d = Detector::new();
        let mut trace = hold(0, 3000, vec![zoom(true, true)]);
        // Fullscreen on another Space or hidden: Accessibility lists nothing.
        trace.extend(hold(
            4000,
            30_000,
            vec![AppSeen::new(ZOOM, false, Some(vec![]))],
        ));
        assert_eq!(
            run(&mut d, &trace),
            ["2000 Zoom start", "12000 Zoom maybe ended"]
        );
    }

    #[test]
    fn decline_window_watching_follows_the_session() {
        let mut d = Detector::new();
        assert!(d.wants_windows(ZOOM, true));
        assert!(!d.wants_windows(ZOOM, false));
        assert!(!d.wants_windows("com.example.unknown", true));
        d.update(0, &[zoom(true, true)]);
        d.update(2000, &[zoom(true, true)]);
        assert!(
            d.wants_windows(ZOOM, false),
            "a live session keeps watching"
        );
        assert!(d.wants_poll(false));
    }
}
