# iced spike notes

Working notes for `docs/spike-ui-toolkit-results.md`. Linux (Hyprland,
Wayland, 1.6× scale) only so far. Same fixtures, features and flags as
`spikes/gpui`.

## Stack

- `iced` 0.14 (crates.io) with `advanced`, `image`, `tokio`.
- Message list: custom `VirtualList` widget (`src/virtual_list.rs`): rows built
  during layout, heights cached, scroll anchored to an item.
- Text: custom `SelectableText` widget (`src/selectable_text.rs`): one
  paragraph per source line, drag selection, copy, clickable links.
- Composer: `text_editor` with a key binding for Enter / Shift+Enter; paste
  routed through `arboard` to detect images.
- Async: tokio thread → `mpsc` → `Subscription::run_with` stream; decoding via
  `Task::perform` + `spawn_blocking`.
- Extra crates for platform gaps: `arboard` (image clipboard), `rfd` (file
  dialog); links open via `xdg-open` (Linux only).

## Run

```sh
nix develop
cd spikes/iced
cargo run --release              # follows system theme
cargo run --release -- --feed    # live feed at 50 msg/s
cargo run --release -- --jump 8945 --theme "Catppuccin Mocha"
cargo run --release -- --bench-scroll   # fling newest → oldest, prints frame times
cargo test
```

Debug toggles: `SPIKE_PLAIN_TEXT=1` (iced `text` instead of
`SelectableText`), `SPIKE_NO_IMAGES=1`.

## Measurements (Linux, release)

| Metric | Result |
|---|---|
| Start to first frame | 158 ms median of 5 (156–160); taken at the first redraw request, before drawing |
| Idle RAM | 129 MiB RSS at 10 s |
| RAM after full scroll | 347 MiB; 255 MiB with `MALLOC_ARENA_MAX=2`; 328 MiB without images |
| Idle CPU | ~0.0% over 30 s (1 tick) |
| Scroll smoothness | 3015 frames, p50 16.7 / p95 20.8 / p99 30.0 / max 136.8 ms; 82 frames > 25 ms |
| Scroll under 50 msg/s feed | p50 16.7 / p95 25.7 / p99 36.6 / max 138.2 ms; 167 frames > 25 ms |
| Binary size | 22.7 MiB stripped (30.9 MiB unstripped) |
| Clean build time | 2 m 26 s (`cargo build --release`, 12 threads, warm crate cache) |
| Prototype size | 2353 lines in `src/` (plus 85 test); 910 of them are the two custom widgets |

Scroll bench: identical to GPUI (200 px/frame, 4000-message conversation,
200-message pages, 30 ms latency). Frame counts differ because row heights
differ.

## Findings

Works:

- Custom virtual list meets the requirement: opens at newest, prepends pages
  without viewport jumps, jump to id, tail following under 50 msg/s.
- **RTL and mixed Arabic/Hebrew wrap correctly** (cosmic-text bidi); CJK and
  ZWJ emoji fine.
- Runtime theme switching is trivial (`theme()` returns the current
  `Theme::custom`); system mode via `system::theme_changes()`.
- Elm architecture keeps async plumbing simple: `Task::perform` and
  subscriptions; tokio is iced's executor.
- Headless test with `iced_test::Simulator` (selectors by text and id, typed
  keys); passed first try, no thread-wakeup restrictions.
- Smaller binary and faster clean build than GPUI.

Problems:

- **No virtualized list** in iced or the ecosystem for variable-height rows:
  502 lines of custom widget (layout-time row building, height cache,
  anchoring, own scrollbar without drag).
- **No selectable text**: `text`/`rich_text` cannot be selected; 408 lines of
  custom widget. Paragraph hit tests drop the line index, forcing one
  paragraph per source line.
- **Text shaping on the UI thread dominates frame time**: `perf` shows
  harfrust shape-plan compilation (`LayoutTable::langsys`,
  `find_language_feature`) at the top of the main thread; new rows entering
  view cause scattered 25–45 ms frames. Not caused by images or the custom
  text widget (same with `SPIKE_*` toggles).
- Higher base memory after scrolling than GPUI without images (328 vs 162
  MiB), lower with images (iced evicts undrawn image handles itself).
- No text ellipsis or line clamp: previews and reply quotes are hard-clipped.
- Clipboard is text-only (images need `arboard`); no file dialog (`rfd`); no
  open-URL helper.
- Japanese full-width comma (、) rendered oddly in one bubble; keycap emoji
  (1️⃣) is a box, as in GPUI.

## Manual checks still to do

- [ ] Select and copy within a bubble; click a link.
- [ ] Paste text; paste an image (status line shows dimensions).
- [ ] IME: fcitx5 and IBus (Linux), Japanese (macOS), Microsoft Pinyin
      (Windows).
- [ ] System dark/light switch while running.
- [ ] Fractional scaling: Wayland 125% and 150%, Windows 150%, macOS Retina.
- [ ] X11 session; Wayland CSD vs SSD.
- [ ] macOS and Windows builds and measurements.
