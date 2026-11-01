{
  description = "Aqueous: an archive format and archiver built around lossless recompression";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # aqr-flate, aqr-png and aqr-pdf have flate-zoo (a submodule) as a dev-dependency, which Cargo
  # resolves even when building without the tests. An input of its own rather than
  # `self.submodules`, which wants the submodule checked out; keep the rev the submodule's.
  inputs.flate-zoo = {
    url = "github:lifthrasiir/flate-zoo/2724c73c3efe336ee66db62dc188b5189a38149a";
    flake = false;
  };

  outputs =
    {
      self,
      nixpkgs,
      flate-zoo,
    }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          manifest = (pkgs.lib.importTOML ./crates/aqr/Cargo.toml).package;
        in
        rec {
          aqr = pkgs.rustPlatform.buildRustPackage {
            pname = manifest.name;
            inherit (manifest) version;
            __structuredAttrs = true;
            src = pkgs.runCommand "aqueous-source" { } ''
              cp -r --no-preserve=mode ${self} $out
              mkdir -p $out/flate-zoo
              cp -r --no-preserve=mode ${flate-zoo}/. $out/flate-zoo
            '';
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "aqr" ];
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
          default = aqr;
        }
      );
    };
}
