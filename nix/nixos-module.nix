# NixOS module: installs Copiale, with `settings` as system-wide defaults
# in /etc/xdg/copiale/config.toml, and `autostart` as an XDG autostart entry.
self: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.copiale;
  settings = import ./settings.nix {inherit lib;};
  autostartEntry = pkgs.makeDesktopItem {
    name = "page.codeberg.korba.Copiale";
    desktopName = "Copiale";
    comment = "Starts Copiale hidden";
    exec = "${lib.getExe cfg.package} --hidden";
    icon = "page.codeberg.korba.Copiale";
    terminal = false;
    noDisplay = true;
  };
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

    autostart = lib.mkEnableOption ''
      starting Copiale hidden at login, through an entry in
      /etc/xdg/autostart. Desktops such as GNOME and KDE run it; Hyprland
      and sway only through a session manager such as UWSM. Users opt out
      with a `page.codeberg.korba.Copiale.desktop` in ~/.config/autostart
      that sets `Hidden=true`
    '';
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    environment.etc."xdg/copiale/config.toml" = lib.mkIf (settings.prune cfg.settings != {}) {
      source = settings.toml pkgs cfg.settings;
    };
    environment.etc."xdg/autostart/page.codeberg.korba.Copiale.desktop" = lib.mkIf cfg.autostart {
      source = "${autostartEntry}/share/applications/page.codeberg.korba.Copiale.desktop";
    };
  };
}
