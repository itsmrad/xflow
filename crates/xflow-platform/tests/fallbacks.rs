//! Fake helpers live in a child process whose PATH contains only stubs.
use std::{path::PathBuf, process::Command, time::Duration};
use xflow_core::{
    config::{InjectionConfig, InjectionMethod, SoundsConfig},
    Desktop, InjectionOutcome,
};
use xflow_platform::{
    sounds::{self, Cue},
    LinuxDesktop,
};

#[test]
#[ignore = "run on isolated bus with displays unset and --test-threads=1"]
fn fake_helpers_cover_x11_wayland_and_wtype() {
    assert!(std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none());
    assert!(std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some());
    use std::os::unix::fs::PermissionsExt;
    for (index, mode) in ["x11", "wayland-ydotool", "wayland-wtype"]
        .into_iter()
        .enumerate()
    {
        let root = std::env::temp_dir().join(format!("xp-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = r##"#!/bin/sh
root=$XFLOW_PLATFORM_TEST_ROOT
tool=${0##*/}
printf '%s\n' "$tool $*" >> "$root/calls"
case "$tool" in
wl-copy|wl-paste|xclip)
    file="$root/clipboard"
    case " $* " in *primary*) file="$root/primary";; esac
    case "$tool $*" in
        wl-paste*|*' -o '*) /bin/cat "$file";;
        *) /bin/cat > "$file";;
    esac;;
xdotool)
    case "$1" in
        getactivewindow) printf '42\n';;
        getwindowclassname) /bin/cat "$root/app";;
        type) /bin/cat > "$root/typed";;
        key) :;;
    esac;;
swaymsg) printf '{"focused":true,"id":42,"app_id":"%s"}' "$(/bin/cat "$root/app")";;
ydotool)
    if [ "$1" = type ]; then /bin/cat > "$root/typed"; fi;;
wtype)
    if [ "$1" = '-' ]; then /bin/cat > "$root/typed"; fi;;
pw-play|paplay|aplay)
    for last in "$@"; do :; done
    if [ -f "$last" ]; then /bin/cat "$last" > "$root/cue"; else /bin/cat > "$root/cue"; fi;;
esac
"##;
        for name in [
            "wl-copy", "wl-paste", "xclip", "xdotool", "swaymsg", "ydotool", "wtype", "pw-play",
            "paplay", "aplay",
        ] {
            let file = root.join(name);
            std::fs::write(&file, script).unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(root.join("clipboard"), "original clipboard\n").unwrap();
        std::fs::write(root.join("primary"), "  selection\n").unwrap();
        std::fs::write(root.join("app"), "editor").unwrap();
        let socket = root.join("keyboard.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "fallback_fixture", "--ignored", "--nocapture"])
            .env("PATH", &root)
            .env("XFLOW_PLATFORM_TEST_ROOT", &root)
            .env("XFLOW_PLATFORM_TEST_SESSION", mode)
            .env("XDG_RUNTIME_DIR", &root)
            .env(
                "YDOTOOL_SOCKET",
                if mode == "wayland-ydotool" {
                    socket
                } else {
                    root.join("absent.sock")
                },
            )
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY");
        if mode == "x11" {
            child.env("DISPLAY", "fake-display");
        } else {
            child
                .env("WAYLAND_DISPLAY", "fake-wayland")
                .env("SWAYSOCK", root.join("fake-sway"));
        }
        let status = child.status().unwrap();
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
        assert!(status.success(), "{mode}");
    }
}

async fn wait_for_file(root: &std::path::Path, file: &str, expected: &[u8]) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::read(root.join(file)).is_ok_and(|data| data == expected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "child fixture only; parent supplies fake PATH and isolated bus"]
async fn fallback_fixture() {
    let Some(root) = std::env::var_os("XFLOW_PLATFORM_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(std::env::var_os("PATH").unwrap(), root.as_os_str());
    let mode = std::env::var("XFLOW_PLATFORM_TEST_SESSION").unwrap();
    let config = InjectionConfig {
        restore_delay_ms: 25,
        ..Default::default()
    };
    let desktop = LinuxDesktop::new(&config);
    let target = desktop.context().await.unwrap();
    assert_eq!(target.window_id.as_deref(), Some("42"));
    assert_eq!(
        desktop.selection().await.unwrap().as_deref(),
        Some("  selection\n")
    );
    let now = std::time::Instant::now();
    assert_eq!(
        desktop.inject("hello", &target).await.unwrap(),
        InjectionOutcome::Pasted
    );
    eprintln!("fake {mode} paste dispatch: {:?}", now.elapsed());
    wait_for_file(&root, "clipboard", b"original clipboard\n").await;
    assert_eq!(
        desktop.inject("another", &target).await.unwrap(),
        InjectionOutcome::Pasted
    );
    std::fs::write(root.join("clipboard"), "user copied something else").unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::read_to_string(root.join("clipboard")).unwrap(),
        "user copied something else"
    );
    let typed = LinuxDesktop::new(&InjectionConfig {
        method: InjectionMethod::Type,
        ..config.clone()
    });
    assert_eq!(
        typed.inject("-héllo\nworld", &target).await.unwrap(),
        InjectionOutcome::Typed
    );
    assert_eq!(
        std::fs::read_to_string(root.join("typed")).unwrap(),
        "-héllo\nworld"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("clipboard")).unwrap(),
        "user copied something else"
    );
    let mut changed = target.clone();
    changed.window_id = Some("different".into());
    assert_eq!(
        desktop.inject("recover me", &changed).await.unwrap(),
        InjectionOutcome::ClipboardOnly
    );
    assert_eq!(
        std::fs::read_to_string(root.join("clipboard")).unwrap(),
        "recover me"
    );
    std::fs::write(root.join("app"), "org.gnome.Ptyxis").unwrap();
    let terminal = desktop.context().await.unwrap();
    assert_eq!(
        desktop.inject("echo safe", &terminal).await.unwrap(),
        InjectionOutcome::Pasted
    );
    wait_for_file(&root, "clipboard", b"recover me").await;
    let before = std::fs::read_to_string(root.join("calls")).unwrap();
    assert_eq!(
        desktop.inject("echo unsafe\n", &terminal).await.unwrap(),
        InjectionOutcome::ClipboardOnly
    );
    let after = std::fs::read_to_string(root.join("calls")).unwrap();
    let extra = &after[before.len()..];
    assert!(
        !extra.contains("xdotool key")
            && !extra.contains("ydotool key")
            && !extra.contains("wtype ")
    );
    match mode.as_str() {
        "x11" => assert!(before.contains("xdotool key --clearmodifiers ctrl+shift+v")),
        "wayland-ydotool" => assert!(before.contains("ydotool key 29:1 42:1 47:1 47:0 42:0 29:0")),
        _ => assert!(before.contains("wtype -M ctrl -M shift -k v -m shift -m ctrl")),
    }
    sounds::play(
        Cue::Start,
        &SoundsConfig {
            volume: 0.25,
            ..Default::default()
        },
    );
    wait_for_file(&root, "cue", include_bytes!("../assets/start.wav")).await;
    assert!(std::fs::read_to_string(root.join("calls"))
        .unwrap()
        .contains("pw-play --volume 0.25 -"));
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    sounds::play(
        Cue::Stop,
        &SoundsConfig {
            enabled: false,
            ..Default::default()
        },
    );
    sounds::play(
        Cue::Error,
        &SoundsConfig {
            volume: 0.0,
            ..Default::default()
        },
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(std::fs::read_to_string(root.join("calls")).unwrap(), calls);
    std::fs::remove_file(root.join("pw-play")).unwrap();
    std::fs::remove_file(root.join("cue")).unwrap();
    let custom = root.join("custom.wav");
    std::fs::write(&custom, include_bytes!("../assets/stop.wav")).unwrap();
    sounds::play(
        Cue::Stop,
        &SoundsConfig {
            stop: Some(custom),
            volume: 0.25,
            ..Default::default()
        },
    );
    wait_for_file(&root, "cue", include_bytes!("../assets/stop.wav")).await;
    assert!(std::fs::read_to_string(root.join("calls"))
        .unwrap()
        .contains("paplay --volume=16384 -- "));
    std::fs::remove_file(root.join("paplay")).unwrap();
    std::fs::remove_file(root.join("cue")).unwrap();
    let mut attenuated = include_bytes!("../assets/error.wav").to_vec();
    for sample in attenuated[44..].as_chunks_mut::<2>().0 {
        let value = i16::from_le_bytes(*sample);
        *sample = ((f32::from(value) * 0.25).round() as i16).to_le_bytes();
    }
    sounds::play(
        Cue::Error,
        &SoundsConfig {
            volume: 0.25,
            ..Default::default()
        },
    );
    wait_for_file(&root, "cue", &attenuated).await;

    if mode == "x11" {
        std::fs::write(root.join("app"), "xterm").unwrap();
        let terminal = desktop.context().await.unwrap();
        let primary = std::fs::read(root.join("primary")).unwrap();
        let clipboard = std::fs::read(root.join("clipboard")).unwrap();
        assert_eq!(
            desktop
                .inject("traditional terminal", &terminal)
                .await
                .unwrap(),
            InjectionOutcome::Pasted
        );
        wait_for_file(&root, "primary", &primary).await;
        wait_for_file(&root, "clipboard", &clipboard).await;
        assert!(std::fs::read_to_string(root.join("calls"))
            .unwrap()
            .contains("shift+Insert"));
    }
    if mode == "wayland-ydotool" {
        std::fs::remove_file(root.join("ydotool")).unwrap();
        let terminal = typed.context().await.unwrap();
        assert_eq!(
            typed
                .inject("wtype when ydotool is absent", &terminal)
                .await
                .unwrap(),
            InjectionOutcome::Typed
        );
        assert_eq!(
            std::fs::read_to_string(root.join("typed")).unwrap(),
            "wtype when ydotool is absent"
        );
    }
}
