# Changes from upstream vt100 0.16.2

Upstream: https://github.com/doy/vt100-rust (MIT, see LICENSE).

1. **Compact scrollback rows** (`src/grid.rs`, `scroll_up`): when a line scrolls
   into the scrollback, trailing blank cells (default contents and colours) are
   dropped and the row is copied into a right-sized allocation (freeing the
   full-width one whole, so the allocator reuses it for the next line). A cell is 32 bytes, so a short
   line in a wide terminal used to cost several KB; most of that is gone.
   Rows always keep at least one cell, and every reader already treats a
   missing cell as blank (`Row::get` returns `None`, contents iterate `cells()`).

2. **Scrollback access** (`src/grid.rs`, `src/screen.rs`): `Screen::all_lines()`
   and `Screen::scrollback_lines()` for searching a pane's history, and ED 3
   (`ESC [ 3 J`, what `clear` sends) clears the scrollback like xterm and
   Terminal.app.

3. **No panics on odd input** (`src/grid.rs` `col_wrap`, `src/screen.rs`
   `text`, `src/row.rs` `resize`), found by Shellvisor's random-input tests:
   a wide character on a one-column screen is dropped instead of breaking the
   cell invariants; a row made narrower clears a wide character cut in half;
   wrapping after a resize with a stale scroll region saturates instead of
   underflowing.
