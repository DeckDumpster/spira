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
    in {
      packages.${system} = {
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
            nodejs
            openssl
          ];
        };
        default = self.packages.${system}.spira-env;
      };
    };
}
