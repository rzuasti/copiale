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

    # The Nix package targets Linux; macOS ships as a notarized .dmg.
    forEachLinuxSystem = nixpkgs.lib.genAttrs (builtins.filter (nixpkgs.lib.hasSuffix "-linux") supportedSystems);
  in {
    packages = forEachLinuxSystem (system: rec {
      copiale = pkgsBySystem.${system}.callPackage ./nix/package.nix {};
      default = copiale;
    });

    # `programs.copiale`: the package, and settings as system-wide defaults.
    nixosModules.default = import ./nix/nixos-module.nix self;

    # `programs.copiale`: the package, and settings as the user's config.
    homeModules.default = import ./nix/home-module.nix self;
    homeManagerModules = self.homeModules;

    # `nix run .#release -- <version>`; see docs/release-macos.md.
    # `nix run .#windows-vm -- <command>`; see docs/windows-vm.md.
    apps = forEachSystem (system:
      {
        release = {
          type = "app";
          program = nixpkgs.lib.getExe (pkgsBySystem.${system}.callPackage ./nix/release.nix {});
        };
      }
      // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
        windows-vm = {
          type = "app";
          program = nixpkgs.lib.getExe (pkgsBySystem.${system}.callPackage ./nix/windows-vm.nix {});
        };
      });

    checks = forEachLinuxSystem (system: let
      pkgs = pkgsBySystem.${system};
    in {
      modules = import ./nix/tests.nix {
        inherit nixpkgs pkgs;
        copiale = self.packages.${system}.default;
        nixosModule = self.nixosModules.default;
        homeModule = self.homeModules.default;
      };

      release = let
        src = nixpkgs.lib.fileset.toSource {
          root = ./packaging;
          fileset = nixpkgs.lib.fileset.unions [./packaging/release.sh ./packaging/release.bats];
        };
      in
        pkgs.runCommand "release-tests" {nativeBuildInputs = [pkgs.bats pkgs.git];} ''
          HOME=$TMPDIR bats ${src}/release.bats
          touch $out
        '';
    });

    # Development shell to build and test the app locally
    devShells = forEachSystem (system: let
      pkgs = pkgsBySystem.${system};

      linuxRuntimeLibs = import ./nix/runtime-libs.nix pkgs;

      # fcitx5 with Japanese and Chinese input, for manual IME testing.
      fcitx5 = pkgs.qt6Packages.fcitx5-with-addons.override {
        addons = [pkgs.fcitx5-mozc pkgs.qt6Packages.fcitx5-chinese-addons];
      };

      # Runs fcitx5 with a throwaway profile: the given keyboard layout plus
      # one input method; Ctrl+Space toggles between them.
      imeTest = pkgs.writeShellApplication {
        name = "ime-test";
        runtimeInputs = [fcitx5];
        text = ''
          case "''${1:-}" in
            ja) im=mozc ;;
            zh) im=pinyin ;;
            *) echo "usage: ime-test ja|zh [fcitx5-keyboard, default keyboard-us]" >&2; exit 2 ;;
          esac
          keyboard="''${2:-keyboard-us}"
          config=$(mktemp -d)
          trap 'rm -rf "$config"' EXIT
          mkdir -p "$config/fcitx5"
          cat > "$config/fcitx5/profile" <<EOF
          [Groups/0]
          Name=Default
          Default Layout=''${keyboard#keyboard-}
          DefaultIM=$im

          [Groups/0/Items/0]
          Name=$keyboard
          Layout=

          [Groups/0/Items/1]
          Name=$im
          Layout=

          [GroupOrder]
          0=Default
          EOF
          echo "fcitx5 running with $im; Ctrl+Space toggles it. Ctrl+C to stop."
          XDG_CONFIG_HOME="$config" fcitx5 --replace
        '';
      };
    in {
      # Default shell plus fcitx5: `nix develop .#ime`, then `ime-test ja|zh`.
      ime = pkgs.mkShell {
        inputsFrom = [self.devShells.${system}.default];
        packages = [imeTest];
        inherit (self.devShells.${system}.default) DEV_SHELL RUST_SRC_PATH RUST_BACKTRACE LD_LIBRARY_PATH;
      };

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
            (callPackage ./nix/ffmpeg.nix {}) # video previews and playback
          ]
          ++ lib.optionals stdenv.hostPlatform.isLinux (linuxRuntimeLibs ++ [alsa-lib]);

        LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (pkgs.lib.makeLibraryPath linuxRuntimeLibs);
      };
    });
  };
}
