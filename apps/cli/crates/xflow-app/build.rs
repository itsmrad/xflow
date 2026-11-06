// The meeting consent/indicator/tap helper is Objective-C (Core Audio process
// taps, AppKit opened at run time). It links into `xflow` only, never xflowd.
fn main() {
    println!("cargo:rerun-if-changed=src/cli/meeting_native.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let object = out.join("meeting_native.o");
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        _ => "x86_64",
    };
    let status = std::process::Command::new("clang")
        .args(["-c", "-fobjc-arc", "-O2", "-Wall", "-Werror"])
        .args([
            "-Wunguarded-availability-new",
            "-mmacosx-version-min=11.0",
            "-arch",
            arch,
        ])
        .arg("src/cli/meeting_native.m")
        .arg("-o")
        .arg(&object)
        .status()
        .expect("clang is required to build xflow on macOS");
    assert!(status.success(), "meeting_native.m failed to compile");
    println!("cargo:rustc-link-arg-bin=xflow={}", object.display());
    // AppKit is opened at run time by the helper, so other commands never load it.
    for framework in ["Foundation", "CoreAudio"] {
        println!("cargo:rustc-link-arg-bin=xflow=-framework");
        println!("cargo:rustc-link-arg-bin=xflow={framework}");
    }
}
