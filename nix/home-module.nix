# Home Manager module: installs Copiale, with `settings` as the user's
# config.toml, and `autostart` as a systemd user service.
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

    autostart = lib.mkEnableOption ''
      starting Copiale hidden with the graphical session, as a systemd user
      service bound to graphical-session.target. The session must reach
      that target with the display variables in systemd's environment
    '';
  };

  config = lib.mkIf cfg.enable {
    home.packages = [cfg.package];
    xdg.configFile."copiale/config.toml" = lib.mkIf (settings.prune cfg.settings != {}) {
      source = settings.toml pkgs cfg.settings;
    };
    systemd.user.services.copiale = lib.mkIf cfg.autostart {
      Unit = {
        Description = "Copiale, a desktop client for Signal";
        PartOf = ["graphical-session.target"];
        After = ["graphical-session.target"];
      };
      # Exits at once if already running.
      Service = {
        ExecStart = "${lib.getExe cfg.package} --hidden";
        Restart = "on-failure";
      };
      Install.WantedBy = ["graphical-session.target"];
    };
  };
}
