# Spike: UI toolkit (GPUI vs iced)

## Goal

Pick the UI toolkit by building the same throwaway prototype in both candidates
and comparing them on chat-app essentials, performance and dev ergonomics.

## Candidates

- **GPUI** + [gpui-component](https://github.com/longbridge/gpui-component)
- **iced**

Fallback if both fail a hard requirement: **Slint**.

## Timebox

- 2 days per toolkit.
- 0.5 day to write results.

## Layout

```
spikes/
  fixtures/   # shared fake-data generator (crate)
  gpui/       # GPUI prototype
  iced/       # iced prototype
```

- Not part of the main workspace; deleted or archived after the decision.
- No presage or Signal integration; data comes from `fixtures`.

## Fixtures

Generated and deterministic (seeded), shared by both prototypes:

- 10k messages across 20 conversations.
- Varied lengths: one-word, multi-paragraph, long unbroken URLs.
- Scripts: Latin, CJK, RTL (Arabic/Hebrew), color emoji, ZWJ sequences.
- ~5% messages with an image of random aspect ratio.
- Mixed senders, timestamps, reactions and replies (rendered as plain text).

## Prototype scope

Same UI in both:

- Sidebar with conversation list; main pane with message list and composer.

### 1. Message list

- Virtualized list with variable-height rows.
- Opens scrolled to the newest message.
- Load older messages on scroll-up without the viewport jumping.
- Jump to an arbitrary message by id.

### 2. Text

- Select and copy text within a message.
- Clickable links.
- Correct shaping for CJK, RTL and emoji.

### 3. Composer

- Multi-line; Enter sends, Shift+Enter inserts a newline.
- Paste text; paste image from the clipboard (detect only).
- IME:
  - Linux: fcitx5 and IBus
  - macOS: Japanese input
  - Windows: Microsoft Pinyin

### 4. Theming

- Load a theme from a TOML file.
- Switch themes at runtime without a restart.
- Follow the system light/dark mode.

### 5. Async

- A tokio task pushes simulated incoming messages at 50 msg/s to the open
  conversation.
- The UI must stay responsive while it runs.
- This validates the bridging pattern presage will need.

### 6. Images

- Decode off the UI thread.
- Show a placeholder at the correct size until loaded, so there is no layout
  shift.

### 7. Platform behavior

- Fractional scaling:
  - Wayland: 125% and 150%
  - Windows: 150%
  - macOS: Retina
- Wayland (CSD/SSD) and X11 on Linux.
- Builds and runs from a Nix flake devShell.

### 8. Testability

- One headless view/state test per prototype, using the toolkit's harness
  (`#[gpui::test]` / `iced_test`), e.g. type in the composer, send, and assert
  the message appears in the list.

## Measurements

Per toolkit and OS, release builds:

| Metric | How |
|---|---|
| Cold start to first frame | Timestamp at `main` vs first-frame callback, median of 5 runs |
| Idle RAM | RSS 10 s after start |
| RAM after full scroll | RSS after scrolling through all 10k messages |
| Idle CPU | Average over 30 s, window focused, no input |
| Scroll smoothness | Frame times while flinging through the list (target: 60 fps or higher, no visible hitches) |
| Binary size | Stripped release binary |
| Clean build time | `cargo build --release` from scratch |
| Prototype size | Lines of code (excluding fixtures) |

## Evaluation

Hard requirements (fail = disqualified):

- Virtualized variable-height list without scroll jumps.
- Working IME on all three OSes.
- Runtime theming.
- Responsive UI under async load.

Weighted score (1-5 each):

| Criterion | Weight |
|---|---|
| Text handling (selection, shaping, IME quality) | 25% |
| Performance (startup, RAM, smoothness) | 20% |
| Look and polish achievable | 15% |
| Dev ergonomics and docs | 15% |
| API stability / maintenance risk | 15% |
| Platform integration (scaling, Wayland, Nix) | 10% |

## Deliverables

- `docs/spike-ui-toolkit-results.md`: measurements, scores, notes per OS, and
  the decision.
- Decision recorded in `CLAUDE.md`.
