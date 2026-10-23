# NixOS module: installs Copiale, with `settings` as system-wide defaults
# in /etc/xdg/copiale/config.toml.
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
      System-wide defaults, in /etc/xdg/copiale/config.toml. Each user's
      config.toml, and so the settings UI, overrides them setting by
      setting, and `[theme]` color by color. Applied when Copiale starts.
    '';
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    environment.etc."xdg/copiale/config.toml" = lib.mkIf (settings.prune cfg.settings != {}) {
      source = settings.toml pkgs cfg.settings;
    };
  };
}
