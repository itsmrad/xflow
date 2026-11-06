//! Meeting observer: which meeting apps hold microphone input, read from Core
//! Audio process objects, plus their window titles by Accessibility. Opens no
//! microphone and requests no permission. Runs on its own thread; dropping the
//! handle removes every listener and joins it.
//!
//! Idle it only waits on listeners: default input running (measured: fires on
//! input open and close), default input changed, process list, and service
//! restart. While the device runs, or a meeting is pending or live, it re-reads
//! the process list once a second: with xflow's own mic open, another app's
//! close fires no device callback (measured), and per-process running
//! listeners never fire at all (measured), so they are not used.
use super::{cf_text, AXIsProcessTrusted, AXUIElementCreateApplication, CFArrayGetCount};
use super::{
    AXUIElementSetMessagingTimeout, CFArrayGetValueAtIndex, CFDictionaryGetValue, CFGetTypeID,
    CFNumberGetTypeID, CFNumberGetValue, Owned, Ref,
};
use anyhow::{bail, Result};
use std::{
    collections::HashMap,
    ffi::c_void,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use xflow_core::meeting_detect::{
    outermost_app, AppSeen, Detector, Event, MeetingId, Window, BROWSERS,
};

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}
const SYSTEM: u32 = 1;
const GLOBAL: u32 = fourcc(b"glob");
const PROCESS_LIST: u32 = fourcc(b"prs#");
const DEFAULT_INPUT: u32 = fourcc(b"dIn ");
const RESTARTED: u32 = fourcc(b"srst");
const RUNNING_SOMEWHERE: u32 = fourcc(b"gone");
const PROCESS_PID: u32 = fourcc(b"ppid");
const RUNNING_INPUT: u32 = fourcc(b"piri");
const POLL: Duration = Duration::from_secs(1);

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}
type Listener = unsafe extern "C" fn(u32, u32, *const Address, *mut c_void) -> i32;
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioObjectAddPropertyListener(
        object: u32,
        address: *const Address,
        listener: Listener,
        context: *mut c_void,
    ) -> i32;
    fn AudioObjectRemovePropertyListener(
        object: u32,
        address: *const Address,
        listener: Listener,
        context: *mut c_void,
    ) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: Ref,
        path: *const u8,
        len: isize,
        directory: bool,
    ) -> Ref;
    fn CFBundleCreate(allocator: Ref, url: Ref) -> Ref;
    fn CFBundleGetIdentifier(bundle: Ref) -> Ref;
    fn CFURLGetTypeID() -> usize;
    fn CFURLGetString(url: Ref) -> Ref;
}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> Ref;
    static kCGWindowOwnerPID: Ref;
    static kCGWindowName: Ref;
    static kCGWindowLayer: Ref;
}
extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, size: i32) -> i32;
}

fn address(selector: u32) -> Address {
    Address {
        selector,
        scope: GLOBAL,
        element: 0,
    }
}
fn read_u32(object: u32, selector: u32) -> Option<u32> {
    let (mut value, mut size) = (0_u32, 4_u32);
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address(selector),
            0,
            ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    (status == 0).then_some(value)
}
/// Every Core Audio process object, or None when the HAL refuses (restart).
fn process_objects() -> Option<Vec<u32>> {
    let addr = address(PROCESS_LIST);
    let mut size = 0;
    if unsafe { AudioObjectGetPropertyDataSize(SYSTEM, &addr, 0, ptr::null(), &mut size) } != 0 {
        return None;
    }
    let mut ids = vec![0_u32; size as usize / 4];
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM,
            &addr,
            0,
            ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    ids.truncate(size as usize / 4);
    (status == 0).then_some(ids)
}

/// The listener only wakes the observer thread; it reads nothing itself.
/// One observer per process, which is all xflow needs.
static WAKE: Mutex<Option<mpsc::SyncSender<()>>> = Mutex::new(None);
static RESTART: AtomicBool = AtomicBool::new(false);
unsafe extern "C" fn listener(
    _: u32,
    count: u32,
    addresses: *const Address,
    _: *mut c_void,
) -> i32 {
    let fired = unsafe { std::slice::from_raw_parts(addresses, count as usize) };
    if fired.iter().any(|a| a.selector == RESTARTED) {
        RESTART.store(true, Ordering::Relaxed);
    }
    if let Some(wake) = WAKE.lock().ok().as_deref().and_then(Option::as_ref) {
        let _ = wake.try_send(());
    }
    0
}

/// Registered listeners, removed on drop.
#[derive(Default)]
struct Listeners {
    added: Vec<(u32, u32)>,
    device: u32,
    failed: bool,
}
impl Listeners {
    fn add(&mut self, object: u32, selector: u32) {
        let status = unsafe {
            AudioObjectAddPropertyListener(object, &address(selector), listener, ptr::null_mut())
        };
        if status == 0 {
            self.added.push((object, selector));
        } else {
            self.failed = true;
        }
    }
    fn register() -> Self {
        let mut this = Self::default();
        for selector in [PROCESS_LIST, DEFAULT_INPUT, RESTARTED] {
            this.add(SYSTEM, selector);
        }
        this.bind_device();
        this
    }
    /// Follow the default input; true while its "running somewhere" bit is 1,
    /// and also when it cannot be read (poll rather than miss a meeting).
    fn bind_device(&mut self) -> bool {
        let device = read_u32(SYSTEM, DEFAULT_INPUT).unwrap_or(0);
        if device != self.device {
            let old = self.device;
            self.remove(|&(object, selector)| object == old && selector == RUNNING_SOMEWHERE);
            self.device = device;
            if device != 0 {
                self.add(device, RUNNING_SOMEWHERE);
            }
        }
        device == 0 || read_u32(device, RUNNING_SOMEWHERE).is_none_or(|running| running != 0)
    }
    fn remove(&mut self, matches: impl Fn(&(u32, u32)) -> bool) {
        self.added.retain(|entry| {
            if !matches(entry) {
                return true;
            }
            unsafe {
                AudioObjectRemovePropertyListener(
                    entry.0,
                    &address(entry.1),
                    listener,
                    ptr::null_mut(),
                )
            };
            false
        });
    }
}
impl Drop for Listeners {
    fn drop(&mut self) {
        self.remove(|_| true);
    }
}

/// A process's watched parent app.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    bundle: String,
    /// The app's own process, which owns its windows.
    main: i32,
    /// Its outermost `.app` path.
    root: String,
}
/// Per Core Audio process object: its PID and, if watched, its app.
type Identities = HashMap<u32, (i32, Option<Identity>)>;

/// A watched app's processes, from one reconcile.
struct Seen {
    input: bool,
    main: i32,
    root: String,
}

pub struct Observer {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    declined: mpsc::Sender<MeetingId>,
}
impl Observer {
    pub fn decline(&self, meeting: MeetingId) {
        let _ = self.declined.send(meeting);
        if let Some(wake) = WAKE.lock().ok().as_deref().and_then(Option::as_ref) {
            let _ = wake.try_send(());
        }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(wake) = WAKE.lock().ok().as_deref().and_then(Option::as_ref) {
            let _ = wake.try_send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *WAKE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

pub fn observe(mut on_event: impl FnMut(Event) + Send + 'static) -> Result<Observer> {
    if process_objects().is_none() {
        bail!("meeting detection needs macOS 14.2 or later (Core Audio process objects)");
    }
    let (wake, woken) = mpsc::sync_channel(1);
    {
        let mut slot = WAKE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_some() {
            bail!("meeting observer already running");
        }
        *slot = Some(wake);
    }
    let (declined, declines) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let thread = thread::Builder::new()
        .name("xflow-meeting".into())
        .spawn(move || {
            let mut listeners = Listeners::register();
            let mut detector = Detector::new();
            let mut identities = Identities::new();
            let mut known: HashMap<String, (i32, String)> = HashMap::new();
            let mut accessibility = HashMap::new();
            let start = Instant::now();
            let mut running = listeners.bind_device();
            while !stopping.load(Ordering::Relaxed) {
                let poll = listeners.failed || detector.wants_poll(running);
                let woke = if poll {
                    woken.recv_timeout(POLL).or_else(|e| match e {
                        mpsc::RecvTimeoutError::Timeout => Ok(()),
                        mpsc::RecvTimeoutError::Disconnected => Err(()),
                    })
                } else {
                    woken.recv().map_err(|_| ())
                };
                if woke.is_err() {
                    break;
                }
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                if RESTART.swap(false, Ordering::Relaxed) {
                    // coreaudiod restarted: every object id and listener is stale.
                    // Remove the old set first: removal matches on the same callback.
                    drop(std::mem::take(&mut listeners));
                    listeners = Listeners::register();
                    identities.clear();
                }
                for id in declines.try_iter() {
                    detector.decline(id);
                }
                running = listeners.bind_device();
                if !(running || detector.wants_poll(false)) {
                    continue;
                }
                let Some(apps) =
                    reconcile(&detector, &mut identities, &mut known, &mut accessibility)
                else {
                    continue;
                };
                let at = start.elapsed().as_millis() as u64;
                for event in detector.update(at, &apps) {
                    on_event(event);
                }
            }
            drop(listeners);
        })
        .map_err(|error| {
            *WAKE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            error
        })?;
    Ok(Observer {
        stop,
        thread: Some(thread),
        declined,
    })
}

/// One read of who holds input, grouped by watched parent app. None when the
/// process list cannot be read, so a HAL hiccup never looks like an app quitting.
fn reconcile(
    detector: &Detector,
    identities: &mut Identities,
    known: &mut HashMap<String, (i32, String)>,
    accessibility: &mut HashMap<i32, super::AccessibilityGuard<Owned>>,
) -> Option<Vec<AppSeen>> {
    let objects = process_objects()?;
    let own = std::process::id() as i32;
    identities.retain(|object, _| objects.contains(object));
    let mut seen: HashMap<String, Seen> = HashMap::new();
    for object in objects {
        // A stale object ('!obj') fails these reads and is skipped.
        let (Some(input), Some(pid)) = (
            read_u32(object, RUNNING_INPUT),
            read_u32(object, PROCESS_PID),
        ) else {
            continue;
        };
        let pid = pid as i32;
        if pid == own {
            continue;
        }
        // Resolved once per object; a reused PID gets a new object, re-checked anyway.
        let entry = identities
            .entry(object)
            .or_insert_with(|| (pid, identity(pid)));
        if entry.0 != pid {
            *entry = (pid, identity(pid));
        }
        let Some(found) = &entry.1 else {
            continue;
        };
        let app = seen.entry(found.bundle.clone()).or_insert(Seen {
            input: false,
            main: found.main,
            root: found.root.clone(),
        });
        app.input |= input != 0;
    }
    // An app whose objects vanished (coreaudiod restart) is still running if its
    // main process is: only a real quit counts as gone.
    known.retain(|bundle, (main, root)| {
        if !seen.contains_key(bundle) && pid_path(*main).is_some_and(|p| inside(&p, root)) {
            seen.insert(
                bundle.clone(),
                Seen {
                    input: false,
                    main: *main,
                    root: root.clone(),
                },
            );
        }
        seen.contains_key(bundle)
    });
    accessibility.retain(|pid, _| {
        seen.iter()
            .any(|(bundle, app)| app.main == *pid && detector.wants_windows(bundle, app.input))
    });
    let mut apps = Vec::with_capacity(seen.len());
    for (bundle, app) in seen {
        known.insert(bundle.clone(), (app.main, app.root));
        if detector.wants_windows(&bundle, app.input) && unsafe { AXIsProcessTrusted() } {
            if let std::collections::hash_map::Entry::Vacant(entry) = accessibility.entry(app.main)
            {
                // Chromium builds its tree lazily; reuse dictation's restoring guard.
                if let Some(guard) = Owned::new(unsafe { AXUIElementCreateApplication(app.main) })
                    .and_then(super::AccessibilityGuard::enable)
                {
                    entry.insert(guard);
                }
            }
        }
        let windows = detector
            .wants_windows(&bundle, app.input)
            .then(|| windows(app.main, BROWSERS.contains(&bundle.as_str())))
            .flatten();
        apps.push(AppSeen {
            bundle,
            input: app.input,
            windows,
        });
    }
    Some(apps)
}

/// A process's watched parent app: bundle id, main pid and `.app` path. None
/// for xflow, unbundled tools and apps no adapter lists.
fn identity(pid: i32) -> Option<Identity> {
    let path = pid_path(pid)?;
    let root = outermost_app(&path)?.to_owned();
    let bundle = bundle_id(&root)?;
    if bundle.starts_with("com.xflow.") || !Detector::watches(&bundle) {
        return None;
    }
    // Walk up to the app's own process: windows belong to it, not to a helper.
    let mut main = pid;
    for _ in 0..8 {
        let parent = parent_pid(main)?;
        if parent <= 1 || !pid_path(parent).is_some_and(|p| inside(&p, &root)) {
            break;
        }
        main = parent;
    }
    Some(Identity { bundle, main, root })
}

fn inside(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.starts_with('/'))
}
fn pid_path(pid: i32) -> Option<String> {
    let mut buffer = vec![0_u8; 4096];
    let len = unsafe { proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    buffer.truncate(usize::try_from(len).ok().filter(|&n| n > 0)?);
    String::from_utf8(buffer).ok()
}
fn parent_pid(pid: i32) -> Option<i32> {
    // PROC_PIDTBSDINFO: struct proc_bsdinfo is 136 bytes; pbi_ppid is at offset 16.
    let mut info = [0_u8; 136];
    let len = unsafe { proc_pidinfo(pid, 3, 0, info.as_mut_ptr().cast(), info.len() as i32) };
    (len == info.len() as i32).then(|| i32::from_ne_bytes([info[16], info[17], info[18], info[19]]))
}
fn bundle_id(app: &str) -> Option<String> {
    let url = Owned::new(unsafe {
        CFURLCreateFromFileSystemRepresentation(ptr::null(), app.as_ptr(), app.len() as isize, true)
    })?;
    let bundle = Owned::new(unsafe { CFBundleCreate(ptr::null(), url.0) })?;
    cf_text(unsafe { CFBundleGetIdentifier(bundle.0) })
}

/// Window titles (and, for browsers, a web area's URL) by Accessibility. When
/// AX lists no real windows, the titles from `CGWindowListCopyWindowInfo`, but
/// only if Screen Recording is already granted: never a prompt. None: unknown.
fn windows(pid: i32, browser: bool) -> Option<Vec<Window>> {
    if unsafe { AXIsProcessTrusted() } {
        if let Some(found) = ax_windows(pid, browser) {
            return Some(found);
        }
    }
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return None;
    }
    cg_windows(pid)
}
fn ax_windows(pid: i32, browser: bool) -> Option<Vec<Window>> {
    let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) })?;
    unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
    let _ = app.text("AXRole");
    let list = app.attribute("AXWindows")?;
    let mut out = Vec::new();
    for index in 0..unsafe { CFArrayGetCount(list.0) }.min(32) {
        let window = unsafe { CFArrayGetValueAtIndex(list.0, index) };
        // Borrowed from the array; Owned would release it.
        let window = std::mem::ManuallyDrop::new(Owned(window));
        // Some processes get stub elements (role AXApplication) instead of windows.
        if window.text("AXRole").as_deref() != Some("AXWindow") {
            return None;
        }
        out.push(Window {
            title: window.text("AXTitle").unwrap_or_default(),
            url: browser.then(|| web_url(&window, 0, &mut 256)).flatten(),
            in_call: call_state(
                &window,
                0,
                &mut 512,
                Instant::now() + Duration::from_millis(200),
            ),
        });
    }
    Some(out)
}
/// The first AXWebArea's URL, bounded by depth and a node budget.
fn web_url(element: &Owned, depth: u32, budget: &mut u32) -> Option<String> {
    if depth > 6 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    if element.text("AXRole").as_deref() == Some("AXWebArea") {
        let url = element.attribute("AXURL")?;
        if unsafe { CFGetTypeID(url.0) } != unsafe { CFURLGetTypeID() } {
            return None;
        }
        return cf_text(unsafe { CFURLGetString(url.0) });
    }
    let children = element.attribute("AXChildren")?;
    (0..unsafe { CFArrayGetCount(children.0) }.min(16)).find_map(|index| {
        let child = std::mem::ManuallyDrop::new(Owned(unsafe {
            CFArrayGetValueAtIndex(children.0, index)
        }));
        web_url(&child, depth + 1, budget)
    })
}

/// Read controls only; a pre-join preview has no Leave control. A post-call
/// Rejoin screen is positive end evidence even if the browser remains open.
fn call_state(element: &Owned, depth: u32, budget: &mut u32, deadline: Instant) -> Option<bool> {
    if depth > 16 || *budget == 0 || Instant::now() >= deadline {
        return None;
    }
    *budget -= 1;
    if element.text("AXRole").as_deref() == Some("AXButton") {
        for attr in ["AXTitle", "AXDescription", "AXHelp"] {
            if let Some(label) = element.text(attr) {
                match label.to_ascii_lowercase().trim() {
                    "leave call" | "leave meeting" | "leave" | "end call" | "hang up" => {
                        return Some(true)
                    }
                    "rejoin" | "rejoin meeting" | "return to home screen" => return Some(false),
                    _ => (),
                }
            }
        }
    }
    let children = element.attribute("AXChildren")?;
    let mut ended = None;
    for index in 0..unsafe { CFArrayGetCount(children.0) }.min(64) {
        let child = std::mem::ManuallyDrop::new(Owned(unsafe {
            CFArrayGetValueAtIndex(children.0, index)
        }));
        match call_state(&child, depth + 1, budget, deadline) {
            Some(true) => return Some(true),
            Some(false) => ended = Some(false),
            None => (),
        }
    }
    ended
}

fn cg_windows(pid: i32) -> Option<Vec<Window>> {
    // kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements
    let list = Owned::new(unsafe { CGWindowListCopyWindowInfo(1 << 4, 0) })?;
    let number = |dict: Ref, key: Ref| {
        let value = unsafe { CFDictionaryGetValue(dict, key) };
        let mut out = 0_i64;
        (!value.is_null()
            && unsafe { CFGetTypeID(value) == CFNumberGetTypeID() }
            && unsafe { CFNumberGetValue(value, 4, (&mut out as *mut i64).cast()) })
        .then_some(out)
    };
    let mut out = Vec::new();
    for index in 0..unsafe { CFArrayGetCount(list.0) } {
        let dict = unsafe { CFArrayGetValueAtIndex(list.0, index) };
        if number(dict, unsafe { kCGWindowOwnerPID }) != Some(pid.into())
            || number(dict, unsafe { kCGWindowLayer }) != Some(0)
        {
            continue;
        }
        if let Some(title) =
            cf_text(unsafe { CFDictionaryGetValue(dict, kCGWindowName) }).filter(|t| !t.is_empty())
        {
            out.push(Window {
                title,
                url: None,
                in_call: None,
            });
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_TEST: &str = "macos::meeting::tests::identity_child";

    /// Child roles for the identity test, run from copies of this test binary.
    #[test]
    fn identity_child() {
        match std::env::var("XFLOW_MEETING_TEST_CHILD").as_deref() {
            Ok("helper") => std::thread::sleep(Duration::from_secs(10)),
            Ok(helper) => {
                let mut child = std::process::Command::new(helper)
                    .args([CHILD_TEST, "--exact", "--quiet"])
                    .env("XFLOW_MEETING_TEST_CHILD", "helper")
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                println!("HELPER_PID {}", child.id());
                let _ = child.wait();
            }
            Err(_) => {}
        }
    }

    /// A real nested helper inside a throwaway `.app` maps to the outer app.
    #[test]
    fn helper_identity_walks_to_the_outer_app() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("Throwaway.app");
        let helper = outer.join("Contents/Frameworks/Throwaway Helper.app");
        let me = std::env::current_exe().unwrap();
        for (app, id, exe) in [
            (&outer, "us.zoom.xos", "Throwaway"),
            (&helper, "us.zoom.xos.helper", "Throwaway Helper"),
        ] {
            std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            std::fs::write(
                app.join("Contents/Info.plist"),
                format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"),
            )
            .unwrap();
            std::fs::copy(&me, app.join("Contents/MacOS").join(exe)).unwrap();
        }
        let helper_exe = helper.join("Contents/MacOS/Throwaway Helper");
        // The outer "app" starts its helper and waits, like a browser.
        let mut child = std::process::Command::new(outer.join("Contents/MacOS/Throwaway"))
            .args([CHILD_TEST, "--exact", "--nocapture", "--quiet"])
            .env("XFLOW_MEETING_TEST_CHILD", &helper_exe)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let helper_pid: i32 = std::io::BufRead::lines(reader)
            .map_while(Result::ok)
            .find_map(|l| {
                l.strip_prefix("HELPER_PID ")
                    .and_then(|p| p.trim().parse().ok())
            })
            .expect("helper pid");
        let found = identity(helper_pid);
        let _ = std::process::Command::new("kill")
            .arg(helper_pid.to_string())
            .status();
        let _ = child.kill();
        let _ = child.wait();
        let found = found.expect("helper resolves");
        assert_eq!(found.bundle, "us.zoom.xos");
        assert_eq!(found.main, child.id() as i32);
        assert!(found.root.ends_with("Throwaway.app"));
        // Unbundled and unknown processes resolve to nothing.
        assert_eq!(identity(std::process::id() as i32), None);
    }

    /// Explicitly invoked on the isolated local Chrome fixture under desktop.lock.
    #[test]
    #[ignore = "requires a local browser fixture and the desktop lock"]
    fn local_browser_call_controls() {
        let pid: i32 = std::env::var("XFLOW_MEETING_FIXTURE_PID")
            .unwrap()
            .parse()
            .unwrap();
        let expected = std::env::var("XFLOW_MEETING_FIXTURE_STATE").unwrap();
        let expected = match expected.as_str() {
            "joined" => Some(true),
            "ended" => Some(false),
            _ => None,
        };
        let app = Owned::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap();
        let _guard = super::super::AccessibilityGuard::enable(app);
        let windows = ax_windows(pid, true).expect("fixture AX windows readable");
        let fixture = windows
            .iter()
            .find(|w| w.title == "Meet - abc-defg-hij")
            .expect("local fixture title");
        assert_eq!(fixture.in_call, expected);
        assert!(fixture
            .url
            .as_ref()
            .is_some_and(|url| url.starts_with("file:")));
        let mut detector = Detector::new();
        let apps = [AppSeen::new("com.google.Chrome", true, Some(windows))];
        assert!(detector.update(0, &apps).is_empty());
        assert!(
            detector.update(3000, &apps).is_empty(),
            "a local lookalike is not a Google meeting"
        );
    }

    #[test]
    fn observer_starts_and_tears_down() {
        let observer = observe(|_| {}).expect("process objects available");
        assert!(observe(|_| {}).is_err(), "only one observer per process");
        std::thread::sleep(Duration::from_millis(200));
        drop(observer);
        assert!(WAKE.lock().unwrap().is_none(), "listener wake cleared");
    }
}
