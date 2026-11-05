# Mouse selection in the TUI

- **Up:** [surfaces.md](surfaces.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md)
- **Code:** `crates/tui-app/src/lib.rs`, `crates/tui-app/src/surface.rs`
- **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`

The full-screen surface captures the mouse so the wheel can scroll the conversation.
Capturing it is also what stops the terminal from doing its own drag-select, so the screen
does the selection itself: a left-drag over the conversation highlights cells, and releasing
the button copies the selected text. `/mouse` still hands the mouse back for a terminal-native
selection, at the cost of the wheel; Shift-drag is the terminal's own escape hatch and never
reaches the screen.

## 1. What a drag selects

- A left press on a conversation row starts the selection; dragging extends it; release
  copies. A press and release on one cell is a click: it clears the old selection and copies
  nothing. A right-click with a selection copies it again and puts the highlight away; with
  no selection it does nothing.
- The selection is over **display cells**, not bytes or characters. A CJK glyph is two
  columns, and a drag that lands in either half includes the whole glyph.
- Dragging above or below the page clamps to the first or last row; dragging past the right
  edge clamps to the page's width. The status bar, prompt, approval panel and menus are not
  selectable — only the conversation page is.
- The wheel still scrolls, and scrolling clears the selection. While a drag is in progress
  the wheel is ignored so the rows the drag is anchored to do not move.
- Output that arrives after a selection is complete does not change the selected text: the
  selection is anchored to transcript rows, not to screen cells.

## 2. Where a cell maps

The page is a window over `lines`, or over an open fold, or over the live answer. Each draw
records, for every conversation row on screen, its index in that body and its plain text. A
mouse event looks up the row under the pointer and snaps to the glyph in that cell; the logo
and banner rows above the body are not selectable. Columns are clipped to the page width,
which is also how a long line is drawn: the page does not wrap.

## 3. Copy

On release the selected text — lines joined with `\n`, with no padding — takes the same road
as `/copy`, and every road at once: the window system's clipboard when there is a display,
OSC 52 for a terminal that honours it, and a session-named file that always works. The first
is what makes copy work on GNOME Terminal, which silently discards OSC 52; the second is what
reaches a remote session; the file is the honest fallback. The highlight stays after the
release as the feedback that something was copied; a right-click copies the same text again
and drops it, the "done" a terminal gives. Nothing is added to the transcript, because a row
saying "copied" is one more line to scroll past. An empty selection copies nothing.

## 4. What it does not do

No double-click word or line selection, no keyboard-extended selection, and no
scroll-while-dragging. The terminal's own selection remains available through `/mouse` or
Shift-drag.
