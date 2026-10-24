{
  # spira-env — the third-party tools the harness runs, pinned through one nixpkgs revision.
  #
  # Design: brain wiki/projects/spira/designs/locked-system-deps-v3-2026-10-12.md (approved by
  # Ryan 2026-10-12). nixpkgs packages tagged upstream releases with content hashes, so every
  # tool here is an official release (law-third-party-tools-are-official-releases); flake.lock
  # pins the revision, so the host, the round VM, the CI template and the spill AMI resolve the
  # same versions. Upgrades are reviewed changes to flake.lock (or a named override below).
  #
  # Phase 1 (host) carries the tools whose drift has cost us; host basics (git, podman, tmux,
  # python3, coreutils, systemd tools) stay on the host until phase 2 brings the images along.
  description = "spira-env: pinned third-party tools for the Spira harness";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/7fd4004d0511991fff3a0524856a9be09e28c5f0";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      terraformVersion = "1.16.6";
      # nixpkgs' terraform is source-built under the BSL; this is HashiCorp's own release zip,
      # pinned by version and checksum (hashes from terraform_<version>_SHA256SUMS).
      terraform = pkgs.stdenv.mkDerivation {
        pname = "terraform";
        version = terraformVersion;
        src = pkgs.fetchurl {
          url = "https://releases.hashicorp.com/terraform/${terraformVersion}/terraform_${terraformVersion}_linux_amd64.zip";
          hash = "sha256-OANLONAwljmaehJkoUVhqAKbK0lv0fFpRSZW5Tlo8bc=";
        };
        nativeBuildInputs = [ pkgs.unzip ];
        sourceRoot = ".";
        dontConfigure = true;
        dontBuild = true;
        dontFixup = true;
        installPhase = "install -Dm755 terraform $out/bin/terraform";
      };
      # nixpkgs' aerc is a wrapProgram script over a dynamically linked ELF whose interpreter and
      # libraries live in /nix/store, so it cannot run on a machine without that store. The release
      # ships this CGO-off build instead: one static binary, without notmuch.
      aerc-static = pkgs.aerc.overrideAttrs (old: {
        buildInputs = [ ];
        installPhase = ''
          runHook preInstall
          make $makeFlags CGO_ENABLED=0 GOFLAGS="$GOFLAGS" install
          runHook postInstall
        '';
        postFixup = "";
        doCheck = false;
        doInstallCheck = false;
      });
    in {
      packages.${system} = {
        inherit aerc-static;
        spira-env = pkgs.buildEnv {
          name = "spira-env";
          paths = with pkgs; [
            dolt          # 2.4.2 — the SQL server under both stores
            beads         # 1.3.1 — bd, from gastownhall/beads (the official upstream)
            duckdb        # the query layer over run/tsd
            gh
            jq
            zstd
            sccache
            awscli2
            inotify-tools
            aerc
            hunk
            go
            tailscale
            nodejs
            openssl
          ] ++ [ terraform ];
        };
        default = self.packages.${system}.spira-env;
      };
    };
}
