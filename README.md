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

### Windows

Requires Windows 11 (x64). Download `Copiale-<version>.msix` from
[Releases](https://codeberg.org/korba/copiale/releases).

Packages aren't signed yet ([code signing policy](docs/code-signing.md)), so
double-clicking one won't install it. In an administrator PowerShell:

```powershell
Add-AppxPackage -Path $HOME\Downloads\Copiale-<version>.msix -AllowUnsigned
```

There are no automatic updates yet. To update, install the new package the
same way. The first signed package won't replace an unsigned one: remove it
first, then link again.

To start Copiale at login, run it once, then turn it on in Settings → Apps →
Startup. It starts hidden, in the tray.

Copiale keeps its files in its package's folder, so uninstalling deletes your
messages, attachments and settings; only the database key stays in Credential
Manager, useless without the database. Your files are under
`%LOCALAPPDATA%\Packages\page.codeberg.korba.Copiale_*\LocalCache`:
`config.toml` in `Roaming\korba\Copiale\config`, logs in
`Local\korba\Copiale\data`.

## License

Copyright (C) 2026 Ricardo Zuasti

Licensed under the GNU Affero General Public License v3.0. See
[LICENSE](LICENSE).
