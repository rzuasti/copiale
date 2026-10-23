# The `settings` option for config.toml, built from config-schema.json,
# which `cargo test` checks against the app (copiale-ui's nix_schema test).
{lib}: let
  inherit (lib) mkOption types;

  schema = lib.importJSON ./config-schema.json;

  # As GPUI parses them.
  color = types.strMatching "#([0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})";

  typeOf = setting:
    {
      choice = types.enum (setting.choices or []);
      bool = types.bool;
      integer =
        if setting ? min
        then types.ints.between setting.min setting.max
        else types.int;
      float =
        if setting ? min
        then types.numbers.between setting.min setting.max
        else types.number;
      font = types.str;
    }
    .${setting.type};

  settingOption = setting:
    mkOption {
      type = types.nullOr (typeOf setting);
      default = null;
      description =
        setting.description
        + lib.optionalString (setting ? default)
        " App default: `${builtins.toJSON setting.default}`.";
    };

  tables =
    lib.mapAttrs (table: settings:
      mkOption {
        type = types.submodule {
          options = lib.listToAttrs (map (setting: lib.nameValuePair setting.key (settingOption setting)) settings);
        };
        default = {};
        description = "The `[${table}]` table.";
      })
    (lib.groupBy (setting: setting.table) schema.settings);

  theme = mkOption {
    type = types.nullOr (types.submodule {
      options = {
        appearance = mkOption {
          type = types.enum schema.theme.appearances;
          description = "Whether the theme is dark or light.";
        };
        colors =
          lib.listToAttrs (map (token:
            lib.nameValuePair token.name (mkOption {
              type = color;
              description = token.description;
            }))
          schema.theme.colors)
          // {
            nicks = mkOption {
              type = types.nonEmptyListOf color;
              description = schema.theme.nicks;
            };
          };
      };
    });
    default = null;
    description = ''
      The `[theme]` table: the palette `appearance.theme = "custom"` uses.
      Every color is required, as `#rgb`, `#rgba`, `#rrggbb` or
      `#rrggbbaa`. A theme file without its `name` fits, as in the example.
    '';
    example = lib.literalExpression ''
      builtins.removeAttrs (lib.importTOML ./my-theme.toml) ["name"]
    '';
  };

  # Drops unset settings and empty tables.
  prune = value:
    if lib.isAttrs value
    then lib.filterAttrs (_: v: v != null && v != {}) (lib.mapAttrs (_: prune) value)
    else value;
in
  assert !(tables ? theme); {
    inherit schema prune;

    # The option, described by `description`.
    option = description:
      mkOption {
        type = types.submodule {options = tables // {inherit theme;};};
        default = {};
        description = ''
          ${description}

          Every setting of the settings UI, by its table and key in
          config.toml; unset (`null`) ones are left out.
        '';
        example = lib.literalExpression ''
          {
            appearance.theme = "custom";
            typography = {
              ui_font = "Inter";
              size = 15;
            };
            layout.message_style = "bubbles";
            theme = builtins.removeAttrs (lib.importTOML ./my-theme.toml) ["name"];
          }
        '';
      };

    # `settings` as config.toml.
    toml = pkgs: settings: (pkgs.formats.toml {}).generate "copiale-config.toml" (prune settings);
  }
