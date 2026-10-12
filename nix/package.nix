{
  lib,
  pkgs,
  rustPlatform,
  pkg-config,
  patchelf,
}: let
  cargoToml = lib.importTOML ../Cargo.toml;
  runtimeLibs = import ./runtime-libs.nix pkgs;
in
  rustPlatform.buildRustPackage {
    pname = "copiale";
    inherit (cargoToml.workspace.package) version;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../crates
      ];
    };
    cargoLock.lockFile = ../Cargo.lock;
    cargoBuildFlags = ["--package" "copiale"];

    nativeBuildInputs = [pkg-config patchelf];
    buildInputs = runtimeLibs;

    # Tests run in the dev shell (`cargo test --workspace`).
    doCheck = false;

    # Let the binary find the libraries GPUI dlopens.
    postFixup = ''
      patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/copiale
    '';

    meta = {
      description = "A fast, minimalist native desktop client for Signal";
      homepage = "https://codeberg.org/korba/copiale";
      license = lib.licenses.agpl3Only;
      mainProgram = "copiale";
      platforms = lib.platforms.linux;
    };
  }
