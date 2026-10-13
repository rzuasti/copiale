{
  lib,
  pkgs,
  rustPlatform,
  pkg-config,
  patchelf,
  cmake,
  protobuf,
  perl,
  openssl,
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
        ../vendor
      ];
    };
    cargoLock = {
      lockFile = ../Cargo.lock;
      # One hash per git repository.
      outputHashes = {
        "libsignal-core-0.1.0" = "sha256-BCFfeRO+wh84qwZBMFlbPKZKNHEsTsPuWvlk8OdGMnc=";
        "libsignal-service-0.1.0" = "sha256-WVoSHdVUQYwMKBTqttvpJI0g7U4cvq02UNFjWKAPJb4=";
        "libsqlite3-sys-0.36.0" = "sha256-RddpAS3oi3CHao+Z5Y95kbn4sUs3gNeLhxaKROK7tIk=";
        "presage-0.8.0-dev" = "sha256-yVt1p/IEBLi+323p2kxDgGTe/hSxhZY5R11avZl3vj8=";
        "spqr-1.5.3" = "sha256-7uLf2e8ImUZ0z2nzA+JYmoeODtOYyhuHtRHQAb2/FsA=";
      };
    };
    cargoBuildFlags = ["--package" "copiale"];

    nativeBuildInputs = [
      pkg-config
      patchelf
      cmake # libsignal (boring-sys)
      protobuf # libsignal, presage: protoc
      perl # SQLCipher build
      rustPlatform.bindgenHook # boring-sys
    ];
    # OpenSSL: SQLCipher's crypto provider.
    buildInputs = runtimeLibs ++ [openssl];

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
