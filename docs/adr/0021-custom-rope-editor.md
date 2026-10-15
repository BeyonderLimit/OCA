# ADR-0021: Custom rope-based editor widget (A9 spike result)

- Status: Accepted (spike result) · Date: 2026-10-08 · Code: **A9**, open decision #2/#4
- Source: `OCA-architechture.md` A9; `OCA-implementation.md` §1 decision 2, Phase 1

## Context

Phase 1 needs a TUI editor pane. Architecture A9 turns the widget choice into a
time-boxed spike against a written checklist: **undo/redo, soft wrap, selection,
clipboard, mouse, 50k-word file stays smooth, wide characters**.

## Options

1. **`tui-textarea` 0.7** — the community ratatui textarea widget.
2. **Custom rope editor** — `ropey` buffer + `unicode-width`/`unicode-segmentation`
   in `oca-tui/src/editor.rs`.

## Spike result: tui-textarea rejected

Verified against the `tui-textarea` 0.7.0 sources: it has **no soft wrap at
all** — long lines are clipped at the viewport edge with no word wrapping.
Hard fail for prose writing, the product's core. (No other checklist item could
compensate for that, so the spike stopped there.)

## Decision

Build a custom editor in `oca-tui/src/editor.rs`, scoped exactly to the A9
checklist. Workspace deps: `ropey`, `unicode-width`, `unicode-segmentation`
(`tui-textarea` removed from the workspace manifest).

## Checklist results

| Item | Result |
| --- | --- |
| Undo/redo | Full `Rope` snapshots (ropey clones are O(1), structurally shared); burst coalescing (800 ms) so one typing burst is one undo step; limit 200. |
| Soft wrap | Greedy word wrap by **display width** (`WrapMap`), cached per `(version, width)`; whitespace keeps its place, word-boundary breaks, hard break for unbreakable runs, end-of-line cursor home segment. |
| Selection | Anchor + cursor, shift-selection, normalized ranges; select-all; grapheme-snapped caret placement. |
| Clipboard | Internal copy/cut/paste buffer; `copy()` returns the text so the app layer can also emit OSC52 to the system clipboard (not wired in Phase 1). |
| Mouse | Click/drag caret, wheel scrolling (viewport clamped), editor/tree focus routing. |
| 50k words smooth | See perf below. |
| Wide characters | `unicode-width` for columns, `unicode-segmentation` for grapheme movement; CJK wrap test (`日本語text` at width 4), combining-mark-safe windowed scanning. |

## Performance (this machine, `cargo test -p oca-tui`)

| Fixture | Load | First full wrap | Keystroke (debug) | Keystroke (release) |
| --- | --- | --- | --- | --- |
| 50k words of prose (line breaks) | 0–7 ms | 350 ms / **24 ms** | **0 ms** (incremental) | **0 ms** |
| 50k words, single line (pasted wall) | 0–8 ms | 909 ms / **88 ms** | 915 ms / **89 ms** | **89 ms** |

Two mechanisms make this work:

1. **Incremental splice.** Every edit computes an `EditSpan` against the
   pre-edit buffer: only the touched logical lines are re-wrapped; every later
   segment is renumbered arithmetically (`start/end += delta`, `line += shift`).
   The span models that the line after the old region merges into the new
   region. Undo/redo uses the same path via a common prefix/suffix diff of the
   restored text. A battery test asserts the spliced cache is byte-for-byte
   equal to a full rebuild after every edit path (mid-line insert, hard break,
   join, multi-line paste, mouse caret, CRLF, select-all replace, undo/redo,
   document start/end, multi-line selection delete).
2. **Windowed `wrap_one`.** Wrapping scanned the whole remaining line per
   segment (O(line²) — a single 448k-char line took ~900 ms/88 ms). It now
   scans fixed windows, holding back the window's final grapheme so graphemes
   are never split across windows: O(line) with small constants.

CI assertions live in `fifty_thousand_word_file_stays_responsive` (prose
keystroke must beat a full wrap by 2×) and
`single_line_wall_of_text_stays_bounded` (absolute bounds + no worse than
2× the rebuild it cannot subdivide).

## Consequences

- `oca-tui` carries ~1k lines of editor code (A9's "rope buffer + grapheme
  handling" risk line); the checklist and perf tests are the guardrail.
- Known bound: an edit inside a *single giant logical line* rewraps that whole
  line (~89 ms release at 448k chars). Real Markdown prose wraps per paragraph
  (0 ms keystrokes). Sub-line wrap reuse is a possible future optimization;
  not needed to meet A9.
- Cursor rendering uses a reversed block (no `Modifier::REVERSED`), keeping the
  widget plain `Style`-based for testability with `TestBackend`.
