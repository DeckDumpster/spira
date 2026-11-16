{
  description = "scratch dev shell for deckdumpster (repo-contract phase 1 spike)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/7fd4004d0511991fff3a0524856a9be09e28c5f0";

  outputs = { self, nixpkgs }:
    let
      pkgs = import nixpkgs { system = "x86_64-linux"; };
    in {
      devShells.x86_64-linux.default = pkgs.mkShell {
        packages = [ pkgs.python312 pkgs.uv ];
        UV_PYTHON_DOWNLOADS = "never";
        UV_PYTHON = "${pkgs.python312}/bin/python3.12";
      };
    };
}
