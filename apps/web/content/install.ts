/**
 * Install methods (§7.7). A method whose installer doesn't exist yet is omitted, not shown disabled:
 * no curl | sh, no Homebrew (claims ledger #15), and no .pkg until a GitHub Release publishes one.
 */
export type InstallMethod = { id: string; label: string; lines: { cmd: string; note?: string }[]; caption?: string };

export const INSTALL: InstallMethod[] = [
  {
    id: "macos",
    label: "macOS (source)",
    lines: [
      { cmd: "git clone https://github.com/itsmrad/xflow && cd xflow" },
      { cmd: "cargo build --manifest-path apps/cli/Cargo.toml --release --locked" },
      { cmd: "scripts/install.sh" },
      { cmd: "xflow" },
    ],
    caption: "Plain xflow opens guided setup, then the control centre.",
  },
  {
    id: "linux",
    label: "Linux (beta)",
    lines: [
      { cmd: "sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev wl-clipboard" },
      { cmd: "cargo build --manifest-path apps/cli/Cargo.toml --release --locked" },
      { cmd: "scripts/install.sh" },
      { cmd: "xflow setup" },
    ],
    caption: "GNOME on Wayland also needs the bundled Shell extension: xflow extension install, then log out and back in.",
  },
];
