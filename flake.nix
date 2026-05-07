{
  description = "eevee NEAT player workspace";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    eevee = {
      # url = "github:gastrodon/eevee";
      url = "path:/home/eva/code/eevee";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
      inputs.fenix.follows = "fenix";
      inputs.flake-utils.follows = "flake-utils";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      fenix,
      eevee,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        lib = pkgs.lib;

        toolchain = fenix.packages.${system}.latest.toolchain;
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        # Filter workspace sources: Rust files + embedded ROM + vendored C.
        workspaceSrc = lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            (craneLib.filterCargoSources path type)
            || (builtins.match ".*/nes-tetris/src/data(/.*)?$" path != null)
            || (builtins.match ".*/tetris-c/vendor(/.*)?$" path != null);
        };

        # Assemble the full build tree.
        #
        # The workspace Cargo.toml references eevee as `path = "../eevee"`,
        # which is outside the flake root and therefore inaccessible from
        # the Nix store.  We copy eevee alongside the workspace and patch
        # the path so crane sees a self-contained directory.
        src = pkgs.runCommand "eevee-player-src" { } ''
          cp -rT ${workspaceSrc} $out
          chmod -R u+w $out
          cp -rT ${eevee} $out/eevee
          substituteInPlace $out/Cargo.toml \
            --replace 'path = "../eevee"' 'path = "./eevee"'
        '';

        commonArgs = {
          inherit src;
          strictDeps = true;
          # Skip --all-targets for the dep-only step: the local eevee path dep
          # is now inside the workspace root, so cargo treats its dev/optional
          # targets as in-scope, pulling in deps not present in the lock file.
          # The full per-package builds below still compile every binary target.
          cargoCheckExtraArgs = "";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        mkPkg =
          extra:
          craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
            }
            // extra
          );

      in
      {
        packages = {
          default = mkPkg { };
          nes-tetris = mkPkg { cargoExtraArgs = "-p nes-tetris"; };
          tetris-c = mkPkg { cargoExtraArgs = "-p tetris-c"; };
          parallel = mkPkg { cargoExtraArgs = "--features parallel"; };
          nes-tetris-parallel = mkPkg {
            cargoExtraArgs = "-p nes-tetris --features parallel";
          };
          tetris-c-parallel = mkPkg {
            cargoExtraArgs = "-p tetris-c --features parallel";
          };
        };

        devShells.default = craneLib.devShell { };
      }
    );
}
