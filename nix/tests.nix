# Flake checks: the modules write every setting so that the app reads it
# without problems, and start it hidden with `autostart`.
{
  pkgs,
  nixpkgs,
  copiale,
  nixosModule,
  homeModule,
}: let
  inherit (pkgs) lib;
  inherit (import ./settings.nix {inherit lib;}) schema;

  # Every setting away from its default; a built-in theme as `[theme]`.
  changed = setting:
    {
      choice = lib.last (lib.remove (setting.default or null) setting.choices);
      bool = !setting.default;
      integer = setting.max;
      float = setting.min;
      font = "Inter";
    }
    .${setting.type};
  full =
    lib.foldl' lib.recursiveUpdate {
      theme = builtins.removeAttrs (lib.importTOML ../crates/copiale-ui/themes/nord.toml) ["name"];
    }
    (map (setting: {${setting.table}.${setting.key} = changed setting;}) schema.settings);

  # The modules' config with `programs.copiale` set to `cfg`.
  nixos = cfg:
    (nixpkgs.lib.nixosSystem {
      modules = [
        nixosModule
        {
          nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
          system.stateVersion = "26.05";
          programs.copiale =
            {
              enable = true;
              package = copiale;
            }
            // cfg;
        }
      ];
    })
    .config;

  # Home Manager isn't an input: stubs for the options the module sets.
  home = cfg:
    (lib.evalModules {
      modules = [
        homeModule
        {
          options.home.packages = lib.mkOption {type = lib.types.listOf lib.types.package;};
          options.xdg.configFile = lib.mkOption {
            type = lib.types.attrsOf (lib.types.submodule {
              options.source = lib.mkOption {type = lib.types.path;};
            });
            default = {};
          };
          options.systemd.user.services = lib.mkOption {
            type = lib.types.attrsOf lib.types.attrs;
            default = {};
          };
          config = {
            _module.args.pkgs = pkgs;
            programs.copiale =
              {
                enable = true;
                package = copiale;
              }
              // cfg;
          };
        }
      ];
    })
    .config;

  systemFile = (nixos {settings = full;}).environment.etc."xdg/copiale/config.toml".source;
  userFile = (home {settings = full;}).xdg.configFile."copiale/config.toml".source;
  autostartFile = (nixos {autostart = true;}).environment.etc."xdg/autostart/page.codeberg.korba.Copiale.desktop".source;
  service = (home {autostart = true;}).systemd.user.services.copiale;
in
  # Nothing set: no file, so the settings UI can save.
  assert !((nixos {}).environment.etc ? "xdg/copiale/config.toml");
  assert (home {}).xdg.configFile == {};
  assert (home {}).home.packages == [copiale];
  # No autostart unless asked.
  assert !((nixos {}).environment.etc ? "xdg/autostart/page.codeberg.korba.Copiale.desktop");
  assert (home {}).systemd.user.services == {};
  assert service.Service.ExecStart == "${lib.getExe copiale} --hidden";
  assert service.Install.WantedBy == ["graphical-session.target"];
    pkgs.runCommand "copiale-nix-modules" {} ''
      grep -qx 'Exec=${lib.getExe copiale} --hidden' ${autostartFile}
      for file in ${systemFile} ${userFile}; do
        ${lib.getExe copiale} --check-config "$file"
        ${lib.concatMapStringsSep "\n" (setting: ''
          grep -q '^${setting.key} = ' "$file" || { echo "$file: no ${setting.table}.${setting.key}"; exit 1; }
        '')
        schema.settings}
      done
      cmp ${systemFile} ${userFile}
      touch $out
    ''
