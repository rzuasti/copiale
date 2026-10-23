# Copiale

A fast, minimalist native desktop client for Signal, written in Rust, for
Linux, macOS and Windows.

> **Early days:** Copiale is a work in progress and not yet usable. Don't rely
> on it for your messages.

Copiale is an independent project and is not affiliated with or endorsed by
Signal Messenger LLC.

## Install

### Nix

The flake builds Copiale for Linux, with a desktop entry and icon:

```sh
nix run git+https://codeberg.org/korba/copiale
```

On NixOS, add the flake as an input and enable the module:

```nix
{
  inputs.copiale.url = "git+https://codeberg.org/korba/copiale";

  outputs = {nixpkgs, copiale, ...}: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      modules = [
        copiale.nixosModules.default
        {
          programs.copiale = {
            enable = true;
            # Optional system-wide defaults; users' settings override them.
            settings.layout.density = "compact";
          };
        }
      ];
    };
  };
}
```

With Home Manager, import `copiale.homeModules.default` instead. Its
`programs.copiale.settings` becomes your `config.toml`, which the settings
UI then can't save to: changes made there last until Copiale restarts.

`settings` takes every setting in `config.toml` by table and key, such as
`appearance.theme`, `typography.size` and `window.opacity`, and the colors
of a custom theme:

```nix
programs.copiale.settings = {
  appearance.theme = "custom";
  # A theme file without its name.
  theme = builtins.removeAttrs (lib.importTOML ./my-theme.toml) ["name"];
};
```

`copiale --check-config` lists any problems in the config in effect.

`programs.copiale.autostart = true` starts Copiale hidden at login
(`copiale --hidden`):

- NixOS: an XDG autostart entry, run by desktops such as GNOME and KDE;
  Hyprland and sway run it only through a session manager such as UWSM.
- Home Manager: a systemd user service bound to `graphical-session.target`.

## License

Copyright (C) 2026 Ricardo Zuasti

Licensed under the GNU Affero General Public License v3.0. See
[LICENSE](LICENSE).
