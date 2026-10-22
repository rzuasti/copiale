# Bundled data

## Emoji shortcodes (`emoji_shortcodes.tsv`)

`name<TAB>emoji<TAB>category` for every shortcode, in emoji order; read
by `chat::emoji` for `:shortcodes:`, the composer's suggestions and
the emoji picker.

- **Source:** [emoji-datasource](https://github.com/iamcal/emoji-data)
  16.0.0 `emoji.json` (MIT), the names Signal Desktop uses. Obsoleted
  emoji are left out.
- **Update:**

  ```sh
  curl -LO https://cdn.jsdelivr.net/npm/emoji-datasource@16.0.0/emoji.json
  python3 emoji_shortcodes.py emoji.json emoji_shortcodes.tsv
  ```
