# Home Manager module: installs Copiale, with `settings` as the user's
# config.toml.
self: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.copiale;
  settings = import ./settings.nix {inherit lib;};
in {
  options.programs.copiale = {
    enable = lib.mkEnableOption "Copiale, a desktop client for Signal";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "copiale.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The Copiale package.";
    };

    settings = settings.option ''
      The user's config.toml, in `$XDG_CONFIG_HOME/copiale`. Once any
      setting is set, the file is read-only: changes in the settings UI
      last until Copiale restarts. Unset settings fall back to the system
      defaults (the NixOS module's), then the app's.
    '';
  };

  config = lib.mkIf cfg.enable {
    home.packages = [cfg.package];
    xdg.configFile."copiale/config.toml" = lib.mkIf (settings.prune cfg.settings != {}) {
      source = settings.toml pkgs cfg.settings;
    };
  };
}
