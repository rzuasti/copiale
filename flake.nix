{
  description = "Copiale - A native desktop chat client for Signal";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    # System types to support.
    supportedSystems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];

    # Helper function to generate an attrset '{ x86_64-linux = f "x86_64-linux"; ... }'.
    forEachSystem = nixpkgs.lib.genAttrs supportedSystems;

    # Nixpkgs instantiated for supported system types.
    pkgsBySystem = forEachSystem (system: import nixpkgs {inherit system;});
  in {
    # Development shell to build and test the app locally
    devShells = forEachSystem (system: let
      pkgs = pkgsBySystem.${system};

      # Libraries the UI toolkits (GPUI, iced/winit) dlopen at runtime.
      linuxRuntimeLibs = with pkgs; [
        wayland
        libxkbcommon
        vulkan-loader
        libGL
        libx11
        libxcursor
        libxi
        libxrandr
        libxcb
        fontconfig
        freetype
        dbus
      ];
    in {
      default = pkgs.mkShell {
        DEV_SHELL = "copiale";

        # rust-analyzer: standard library sources
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

        # Pretty backtraces when tests fail
        RUST_BACKTRACE = "1";

        nativeBuildInputs = with pkgs; [
          pkg-config
          cmake # libsignal (boring-sys), GPUI native deps
          protobuf # libsignal/presage: protoc
          perl # vendored OpenSSL (bundled SQLCipher)
          rustPlatform.bindgenHook # libclang for bindgen (boring-sys)
        ];
        buildInputs = with pkgs;
          [
            rustc
            cargo
            clippy # Rust linter
            rustfmt # Rust formatter
            rust-analyzer
            openssl
            sqlcipher # encrypted database; CLI to inspect dev databases
          ]
          ++ lib.optionals stdenv.hostPlatform.isLinux linuxRuntimeLibs;

        LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (pkgs.lib.makeLibraryPath linuxRuntimeLibs);
      };
    });
  };
}
