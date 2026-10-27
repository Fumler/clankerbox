{
  description = "clankerbox: project-aware sandbox wrapper for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      manifest = (lib.importTOML ./Cargo.toml).package;
    in
    {
      packages = forAllSystems (pkgs: rec {
        clankerbox = pkgs.rustPlatform.buildRustPackage {
          pname = manifest.name;
          inherit (manifest) version;
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./templates
              ./tests
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.installShellFiles ];
          postInstall = lib.optionalString (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
            installShellCompletion --cmd cb \
              --bash <($out/bin/cb completions bash) \
              --fish <($out/bin/cb completions fish) \
              --zsh <($out/bin/cb completions zsh)
          '';
          meta = {
            inherit (manifest) description;
            license = lib.licenses.mit;
            mainProgram = "cb";
            platforms = lib.platforms.linux ++ lib.platforms.darwin;
          };
        };
        default = clankerbox;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            clippy
            git-cliff
            rustc
            rustfmt
            rust-analyzer
          ];
        };
      });

      checks = forAllSystems (pkgs: {
        clankerbox = self.packages.${pkgs.stdenv.hostPlatform.system}.clankerbox;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
