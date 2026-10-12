# Spike results: UI toolkit (GPUI vs iced)

Plan: `docs/spike-ui-toolkit.md`. Prototypes: `spikes/gpui`, `spikes/iced`
(details and run instructions in each `NOTES.md`).

**Status:** Linux done; macOS and Windows pending.

## Decision

**GPUI**, with GPUI Kit (`gpui-kit` 0.7.3) as the component layer.

- Smoother scrolling (measured and hands-on), better IME polish.
- Virtual list, selectable text, ellipsis, open-URL and file dialogs come
  built in. iced needed 910 lines of custom widgets for the same features.
- Known gap: wrapped RTL text renders incorrectly. Accepted for now since the
  initial audience reads Western languages; revisit before RTL locales.

Confirm on macOS and Windows (IME, scaling, measurements) before treating
this as final.

## Prototypes

| | GPUI | iced |
|---|---|---|
| Version | `gpui-kit` 0.7.3 (GPUI snapshot `gpui-pre` 0.3.9) | `iced` 0.14 |
| Message list | GPUI `list` + `ListState` | Custom `VirtualList` widget (502 lines) |
| Selectable text | GPUI Kit `TextView` (Markdown; bodies escaped) | Custom `SelectableText` widget (408 lines) |
| Composer | GPUI Kit `Textarea` | `text_editor` + key bindings |
| Paste image | `Textarea::on_paste` | `arboard` |
| File dialog / open URL | Built in | `rfd` / `xdg-open` |
| Async bridge | tokio thread → `mpsc` → `cx.spawn` | tokio thread → `mpsc` → `Subscription` |

Both use the same fixtures, feature set, flags and scroll bench.

## Measurements (Linux, release)

Hyprland, Wayland, 1.6× scale, 12 threads.

| Metric | GPUI | iced |
|---|---|---|
| Start to first frame (median of 5) | 137 ms | 158 ms¹ |
| Idle RAM (RSS at 10 s) | 128 MiB | 129 MiB |
| RAM after full scroll | 657 MiB² | 347 MiB |
| RAM after full scroll, no images | 162 MiB | 328 MiB |
| Idle CPU (30 s) | 0.0% | ~0.0% |
| Scroll p50 / p95 / p99 / max | 16.7 / 17.6 / 18.1 / 30.9 ms | 16.7 / 20.8 / 30.0 / 136.8 ms |
| Frames > 25 ms during scroll | 3 of 4070 | 82 of 3015 |
| Scroll under 50 msg/s feed, p99 | 17.9 ms | 36.6 ms (167 frames > 25 ms) |
| Binary size (stripped) | 40.4 MiB | 22.7 MiB |
| Clean build | 3 m 47 s | 2 m 26 s |
| Prototype size (`src/`) | 1759 lines | 2353 lines |

¹ Taken at the first redraw request, before drawing; GPUI's is after the
first frame, so iced's figure is slightly optimistic.

² About half is glibc arena fragmentation from decoding large fixture PNGs
on worker threads (322 MiB with `MALLOC_ARENA_MAX=2`). A real image
pipeline should decode straight to thumbnail size.

Scroll bench: 200 px/frame from newest to oldest of a 4000-message
conversation, loading 200-message pages with 30 ms simulated latency.

## Hard requirements

| Requirement | GPUI | iced |
|---|---|---|
| Virtualized variable-height list, no scroll jumps | Pass (built in) | Pass (custom widget) |
| Working IME | Pass on Linux (fcitx5); macOS/Windows pending | Pass on Linux (fcitx5); macOS/Windows pending |
| Runtime theming | Pass | Pass |
| Responsive UI under async load | Pass | Pass, with more dropped frames |

## Scores

| Criterion | Weight | GPUI | iced |
|---|---|---|---|
| Text handling (selection, shaping, IME) | 25% | 4 | 3 |
| Performance (startup, RAM, smoothness) | 20% | 5 | 3 |
| Look and polish achievable | 15% | 4 | 3 |
| Dev ergonomics and docs | 15% | 4 | 3 |
| API stability / maintenance risk | 15% | 3 | 4 |
| Platform integration (scaling, Wayland, Nix) | 10% | 4 | 3 |
| **Weighted** | | **4.05** | **3.15** |

- **Text:** GPUI has selection and links built in and its IME looked slightly
  better; RTL wrapping is broken. iced shapes RTL correctly but needed a
  custom selectable widget, has no ellipsis or line clamp, and shapes text on
  the UI thread.
- **Performance:** GPUI held vsync through the bench and the live feed.
  iced's main thread is dominated by text shaping (harfrust shape-plan
  compilation), giving scattered 25–45 ms frames as rows enter view. iced
  uses less memory once images are involved.
- **Polish:** GPUI Kit ships styled components (list items, avatars, inputs,
  scrollbars) and themes; iced needs more hand styling.
- **Ergonomics:** GPUI Kit has thorough docs and agent skills; the entity
  model is more to learn than iced's Elm loop. iced's architecture and test
  harness are simpler, but the missing widgets cost the most time.
- **Stability:** GPUI comes from `gpui-pre` snapshots that may break the API
  between releases (GPUI Kit pins exact versions). iced has regular semver
  releases.
- **Platform:** Both handled Wayland, fractional scaling and system theme.
  iced needed `arboard`, `rfd` and `xdg-open` for clipboard images, file
  dialogs and links.

## Notes per OS

### Linux (Hyprland, Wayland)

Checked manually in both: select and copy, paste text and image, links,
fcitx5 IME (`nix develop .#ime`), system dark/light switch, fractional
scaling at 1.25, 1.5 and 1.6. All passed. GPUI felt much smoother.

Not checked: IBus, X11 session, CSD vs SSD.

### macOS

Pending: Japanese IME, Retina scaling, measurements.

### Windows

Pending: Microsoft Pinyin IME, 150% scaling, measurements.

## Risks and follow-ups for GPUI

- **RTL:** wrapped Arabic/Hebrew lines are positioned outside their box (GPUI
  core). Check upstream status before adding RTL locales.
- **API churn:** pin `gpui-kit` exactly; budget for upgrade work.
- **List scroll handler** fires only for wheel input; drive paging from
  render or visible-range checks.
- **Testing:** GPUI's test scheduler rejects wakeups from foreign threads, so
  the presage bridge must be injectable (a fake source in tests).
- **Plain text** goes through `TextView`'s Markdown parser; Signal's body
  ranges (bold, spoilers) may need a custom text element instead.
- **Emoji:** keycap sequences (1️⃣) render as a box in both toolkits.
- **Binary size:** 40 MiB stripped; trim GPUI Kit features before release.
