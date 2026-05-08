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
      url = "github:gastrodon/eevee/devel";
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
            || (builtins.match ".*/tetris-c/vendor(/.*)?$" path != null)
            || (builtins.match ".*\\.nes$" path != null);
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
          # Exclude eevee from workspace member discovery so that cargo's
          # --all-targets does not try to build eevee's bench/example targets
          # (which have dev-deps not present in this workspace's vendor store).
          substituteInPlace $out/Cargo.toml \
            --replace 'members = ["tetris-c"]' 'members = ["tetris-c"]
exclude = ["eevee"]'
        '';

        commonArgs = {
          inherit src;
          strictDeps = true;
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

        x11Libs = with pkgs; [
          libx11
          libxi
          libxrandr
          libxinerama
          libxcursor
        ];

      in
      {
        packages = {
          default = mkPkg { };
          parallel = mkPkg { cargoExtraArgs = "--features parallel"; };
          x11nes = mkPkg {
            cargoExtraArgs = "--features x11nes";
            buildInputs = x11Libs;
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
          x11nes-parallel = mkPkg {
            cargoExtraArgs = "--features x11nes,parallel";
            buildInputs = x11Libs;
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
        };

        devShells.default = craneLib.devShell { };
      }
    );
}
