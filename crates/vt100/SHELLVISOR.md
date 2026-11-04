# Changes from upstream vt100 0.16.2

Upstream: https://github.com/doy/vt100-rust (MIT, see LICENSE).

1. **Compact scrollback rows** (`src/grid.rs`, `scroll_up`): when a line scrolls
   into the scrollback, trailing blank cells (default contents and colours) are
   dropped and the row is copied into a right-sized allocation (freeing the
   full-width one whole, so the allocator reuses it for the next line). A cell is 32 bytes, so a short
   line in a wide terminal used to cost several KB; most of that is gone.
   Rows always keep at least one cell, and every reader already treats a
   missing cell as blank (`Row::get` returns `None`, contents iterate `cells()`).
