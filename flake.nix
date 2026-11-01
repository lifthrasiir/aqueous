{
  description = "Aqueous: an archive format and archiver built around lossless recompression";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # aqr-flate, aqr-png and aqr-pdf have flate-zoo (a submodule) as a dev-dependency, which Cargo
    # resolves even when building without the tests. An input of its own rather than
    # `self.submodules`, which wants the submodule checked out; keep the rev the submodule's.
    flate-zoo = {
      url = "github:lifthrasiir/flate-zoo/2724c73c3efe336ee66db62dc188b5189a38149a";
      flake = false;
    };
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.flake-parts.flakeModules.easyOverlay
      ];
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      perSystem =
        {
          pkgs,
          system,
          config,
          ...
        }:
        let
          manifest = (pkgs.lib.importTOML ./crates/aqr/Cargo.toml).package;
        in
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [
              (import inputs.rust-overlay)
            ];
            config = { };
          };
          packages.aqr = pkgs.rustPlatform.buildRustPackage {
            pname = manifest.name;
            inherit (manifest) version;
            __structuredAttrs = true;
            src = pkgs.runCommand "aqueous-source" { } ''
              cp -r --no-preserve=mode ${self} $out
              mkdir -p $out/flate-zoo
              cp -r --no-preserve=mode ${inputs.flate-zoo}/. $out/flate-zoo
            '';
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "aqr"
            ];
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.zstd ];
            env.ZSTD_SYS_USE_PKG_CONFIG = true;
            # The codecs' tests want their corpus/ (gitignored) and take long.
            doCheck = false;
            meta = {
              description = "Archiver of the Aqueous format, built around lossless recompression";
              homepage = "https://github.com/lifthrasiir/aqueous";
              license = pkgs.lib.licenses.mit0;
              mainProgram = "aqr";
            };
          };
          packages.default = config.packages.aqr;
          # rust-toolchain.toml's toolchain (the modules' bytes depend on it).
          devShells.default = pkgs.mkShell {
            nativeBuildInputs = [
              (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
              pkgs.pkg-config
            ];
            buildInputs = [ pkgs.zstd ];
          };
          overlayAttrs = {
            aqr = config.packages.aqr;
          };
          formatter = pkgs.nixfmt;
        };
    };
}
