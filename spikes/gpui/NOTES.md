# GPUI spike notes

Working notes for `docs/spike-ui-toolkit-results.md`. Linux (Hyprland,
Wayland, 1.6× scale) only so far.

## Stack

- `gpui-kit` 0.7.3 (crates.io): GPUI snapshot `gpui-pre` 0.3.9 plus GPUI
  Component. One dependency, no git pins.
- Message list: GPUI `list` + `ListState` (variable height, tail follow).
- Text: GPUI Kit `TextView` (Markdown) with escaped bodies; window-level
  selection.
- Composer: GPUI Kit `Textarea` (`auto_grow`, `submit_on_enter`, `on_paste`).
- Async: tokio thread → `tokio::sync::mpsc` → `cx.spawn` loop on the UI
  thread.

## Run

```sh
nix develop
cd spikes/gpui
cargo run --release              # follows system theme
cargo run --release -- --feed    # live feed at 50 msg/s
cargo run --release -- --jump 8945 --theme "Catppuccin Mocha"
cargo run --release -- --bench-scroll   # fling newest → oldest, prints frame times
cargo test
```

Debug toggles: `SPIKE_PLAIN_TEXT=1` (GPUI text instead of `TextView`),
`SPIKE_NO_IMAGES=1`.

## Measurements (Linux, release)

| Metric | Result |
|---|---|
| Start to first frame | 137 ms median of 5 (136–144); dataset generation is 8 ms of it |
| Idle RAM | 128 MiB RSS at 10 s |
| RAM after full scroll | 657 MiB; 322 MiB with `MALLOC_ARENA_MAX=2`; 162 MiB without images |
| Idle CPU | 0.0% over 30 s (no redraws when idle) |
| Scroll smoothness | 4070 frames, p50 16.7 / p95 17.6 / p99 18.1 / max 30.9 ms; 3 frames > 25 ms |
| Scroll under 50 msg/s feed | p50 16.7 / p99 17.9 / max 29.9 ms |
| Binary size | 40.4 MiB stripped (54.1 MiB unstripped) |
| Clean build time | 3 m 47 s (`cargo build --release`, 12 threads, warm crate cache) |
| Prototype size | 1759 lines in `src/` (plus 82 test) |

Scroll bench: 200 px/frame through the 4000-message conversation, loading
200-message pages with 30 ms simulated latency.

## Findings

Works:

- Variable-height virtualized list; opens at newest; prepending older pages
  via `ListState::splice(0..0, n)` keeps the viewport anchored; jump to id.
- Tail following while 50 msg/s arrive; UI frame times unaffected.
- Runtime theme switching via `Theme::update` + `apply_config`; TOML themes
  mapped onto GPUI Kit roles in ~50 lines; system appearance observer.
- Thumbnails decoded on the background executor; fixed-size placeholder box,
  no layout shift.
- Selectable text across bubbles (window-level selection), clickable
  autolinks, long unbroken URLs wrap.
- CJK and ZWJ emoji sequences shape correctly.
- Headless UI test with real key input (`gpui_kit::test::TestWindowExt`).

Problems:

- **RTL wrapping is broken in GPUI core**: wrapped Arabic/Hebrew lines are
  positioned outside their box. Same with plain GPUI text, so not `TextView`.
  Single-line RTL is fine.
- Keycap emoji (1️⃣) render as a box.
- Fallback-font glyphs (CJK, Hebrew) look lighter than Latin text.
- `ListState` scroll handler fires only for wheel input, not scrollbar drags
  or programmatic scrolls; load-older is checked on render instead.
- `TextView` only takes Markdown/HTML: plain message text must be escaped.
  Each visible body is a keyed `TextViewState` entity.
- GPUI's test scheduler panics on wakeups from foreign threads; the network
  bridge must be injectable (`ChatStore::without_feed`).
- Memory after scrolling is dominated by image decode buffers; glibc arenas
  hold roughly half of it.
- `Textarea` has no `.id()` for test lookup; tests focus it via its state.
- Binary is large (icons, GPUI Kit's full component set).

## Manual checks

Scrolling feels much smoother than iced (hands-on, Linux).

- [x] Select and copy across bubbles (works).
- [x] Click a link: opens in the default browser.
- [x] Paste text; paste an image (status line shows the format). (works)
- [x] IME: fcitx5 on Wayland works; looks slightly better than iced.
- [ ] IME: IBus (Linux), Japanese (macOS), Microsoft Pinyin
      (Windows).
- [x] System dark/light switch while running (portal color-scheme): works.
- [x] Fractional scaling on Wayland at 1.25, 1.5 and 1.6: looks fine.
- [ ] Fractional scaling: Windows 150%, macOS Retina.
- [ ] X11 session; Wayland CSD vs SSD.
- [ ] macOS and Windows builds and measurements.
