# Prebuilt xflow release binaries for Nix (x86_64-linux, aarch64-linux): nix run github:itsmrad/xflow
# Bump: set version and both hashes (SHA256SUMS in SRI form: nix hash convert --hash-algo sha256 HEX).
{
  description = "xflow: local-first voice typing (prebuilt release binaries)";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  outputs =
    { self, nixpkgs }:
    let
      version = "0.2.1-beta";
      releases = {
        x86_64-linux = {
          target = "x86_64-unknown-linux-gnu";
          hash = "sha256-Y9LzSw4VmXHTenI/weh4EuEO7GT7CE5T/NxV9j32cu4=";
        };
        aarch64-linux = {
          target = "aarch64-unknown-linux-gnu";
          hash = "sha256-iqKND78QT/l/JCB+Zb6Lsf+DMy+BN1BEAacdMEUi1GU=";
        };
      };
    in
    {
      packages = nixpkgs.lib.mapAttrs (
        system: release:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.stdenv.mkDerivation {
            pname = "xflow";
            inherit version;
            src = pkgs.fetchurl {
              url = "https://github.com/itsmrad/xflow/releases/download/v${version}/xflow-${version}-${release.target}.tar.gz";
              inherit (release) hash;
            };
            nativeBuildInputs = [ pkgs.autoPatchelfHook ];
            buildInputs = [
              pkgs.alsa-lib
              pkgs.dbus
              pkgs.stdenv.cc.cc.lib
            ];
            installPhase = ''
              install -Dm755 -t $out/bin xflow xflowd
              install -dm755 $out/lib/systemd/user
              substitute packaging/xflow.service $out/lib/systemd/user/xflow.service \
                --replace-fail '%h/.local/bin/xflowd' "$out/bin/xflowd"
              install -Dm644 -t $out/share/doc/xflow LICENSE README.md SCOWL-LICENSE.txt BPE-LICENSE.txt
            '';
            meta = {
              description = "Local-first voice typing for Linux desktops";
              homepage = "https://github.com/itsmrad/xflow";
              license = nixpkgs.lib.licenses.mit;
              sourceProvenance = [ nixpkgs.lib.sourceTypes.binaryNativeCode ];
              platforms = [ system ];
              mainProgram = "xflow";
            };
          };
        }
      ) releases;
    };
}
