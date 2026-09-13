# Building a Good Perfect Clear Solver

This document explains how to build a fast, correct **perfect clear (PC)**
solver for Tetris, the algorithms that make it fast, and the trade-offs
involved. It is written from the perspective of the PC search used by the
Fusion engine in `src/search.rs`, but the techniques are general.

A *perfect clear* is a placement sequence that leaves the playfield empty.
Given a starting board and an ordered queue of pieces (with one hold slot),
the solver must find a sequence of piece placements that clears every cell.

---

## 1. Why the naive search fails

The obvious approach is a depth-first search over all legal placements:

```
dfs(board, queue):
    if board empty: return solution
    if queue empty: return failure
    for each legal placement p of queue[0]:
        dfs(board.after(p), queue[1:])
```

This is correct but far too slow. Each piece has roughly 30 legal lock
positions, so the tree grows like `30^depth`. A 6-piece PC is ~7·10⁸ nodes
before pruning; a 10-piece PC is hopeless. The reference solver in
`tetra-tools` spends about 10 ms on a 4-line PC because it does *not* rely on
raw speed alone — it combines several orthogonal ideas:

1. A compact **bitboard** board representation.
2. **Canonical** move generation (deduplicate equivalent placements).
3. **Iterative deepening** so the smallest solution is found first.
4. A **transposition table** to merge equal positions.
5. **Aggressive, sound pruning** that removes whole branches.
6. **Move ordering** so the first branch searched is usually the answer.
7. Optionally, a **precomputed database** of legal boards.
8. Optionally, a **neural network** to order moves.

The rest of this document covers each.

---

## 2. Board representation

Use a **bitboard**: one bit per cell, `row * width + col`. For the common
4-line PC a board is 4×10 = 40 bits, so a single `u64` holds it. This gives:

* `full_row(col)` checks in one AND.
* Placement legality in a handful of shifts/masks.
* O(1) equality and hashing.

Two layouts are useful and should be kept in sync:

* **Rows**: `[u16; 40]` (or `u64` for 4-row boards), convenient for line
  clears and rendering.
* **Columns**: `[u64; 10]`, where bit `y` of column `x` is set iff `(x, y)` is
  filled. Column bitboards make vertical collision checks and hole counting
  trivial.

For example, counting *holes* (empty cells below the top filled cell of a
column) becomes:

```rust
fn count_holes(cols: &[u64; 10]) -> u32 {
    let mut holes = 0;
    for &col in cols {
        if col == 0 { continue; }
        let top = 63 - col.leading_zeros();       // highest filled row
        let below = (!0u64) >> (64 - top);        // rows 0..top
        holes += (below & !col).count_ones();     // empty below the top
    }
    holes
}
```

If you must support boards taller than 64 cells, either use one `u64` per
column (unbounded height) or several `u64` words per row. The column form is
usually preferable because the playfield is only 10 wide.

---

## 3. Move generation

### 3.1 Enumerate lock positions

For a given piece and rotation, compute the set of columns/rows where it can
rest. A clean way is a **collision map** per column: for each column, the
bitmask of rows that are blocked. Then for each rotation, slide the piece
until it collides, and emit the resting position. Fusion already does this in
`movegen.rs` and it is fast (tens of nanoseconds per piece).

If you are starting from scratch, the simplest correct generator is:

```
for rotation in 0..4:
    for x in -2..width:
        drop the piece straight down in column x
        if it does not overlap the board and is supported:
            emit(rotation, x, y)
```

Straight-drop generation misses **spins** (T-spins, all-spins) that require
kicks. For a PC solver, spins are usually not needed to *find* a clear, but if
your game rewards them or your rotation system allows placements unreachable
by straight drops, add kick enumeration.

### 3.2 Canonical placements

Many placements produce the same board. Deduplicate them. `tetra-tools`
returns `Placements::place(...).canonical()`, which collapses placements that
result in the same resulting board. This can cut branching by 20–40%.

### 3.3 Apply and clear

Applying a placement means OR-ing the piece cells into the board, then
removing full rows and compacting. With row bitboards:

```rust
board |= piece_mask;
if any row == FULL_ROW { remove those rows, shift the rest down }
```

Keep the column cache in sync (rebuild after a clear, OR into it on a place).

---

## 4. Search strategies

### 4.1 Iterative-deepening DFS (general boards)

This is what Fusion uses. Search with a depth limit of 1 piece, then 2, then
3, … and return the first success. Iterative deepening gives two things:

* It finds the **smallest** PC (fewest pieces), which is what a player wants.
* It cannot get lost in an enormous deep subtree the way a single full-depth
  DFS can.

Share the transposition table **between** iterations, keyed by the number of
pieces still available (`remaining`). A state searched with `remaining = r`
and proven unsolvable need not be searched again at `remaining <= r`; only
`remaining > r` requires more work. This is the standard depth-limited TT.

### 4.2 Scan / cull / place (4-row boards)

`gomen` in `tetra-tools` uses a breadth-first formulation that is extremely
fast on 4-row boards:

1. **Scan** forward one piece at a time. At each stage keep a map from
   resulting board → (set of queue states that reach it, predecessor boards).
   Prune any board not in the `legal_boards` set (see §7).
2. **Cull** backwards from the final stage, keeping only boards that can reach
   a solution.
3. **Place** forward again, reconstructing concrete solutions from the culled
   set.

The key is that line clears are represented by a **broken board** (see §6)
so board identity is independent of the order lines cleared. This makes the
state space small and the maps cacheable.

### 4.3 Bidirectional / backward search

You can search backwards from the empty board by "un-placing" pieces. This is
rarely worth it because un-placing is ambiguous (many pieces could have
produced a given board). The forward scan/cull/place above is the practical
version of the same idea.

---

## 5. Transposition table

A transposition table (TT) stores results keyed by a hash of the search state.
The state must include **everything that affects the remaining problem**:

* the board,
* the active piece,
* the hold piece,
* the position in the queue (or the multiset of remaining pieces).

The classic bug is forgetting the queue position. Two nodes can share a board
and active piece but have different future queues because one used hold and
the other did not; merging them produces wrong answers.

Use **Zobrist hashing**: assign a random 64-bit key to each (cell) and XOR
them in as cells are set. The board hash then updates incrementally. Mix in
the piece/hold/queue values with distinct constants:

```rust
let hash = zobrist_board
    ^ (current as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    ^ hold_key.wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
    ^ (q_idx as u64 + 1).wrapping_mul(0x1656_67B1_9E37_79F9);
```

Store the **deepest remaining depth already searched** for each hash and skip
a state when `stored >= remaining`. This is sound for finding a solution
within a depth limit and is what makes iterative deepening cheap.

---

## 6. Pruning

Pruning is where the real speed comes from. Only **sound** prunes (that never
remove a real solution) are safe; unsound prunes must be treated as
heuristics and can make the solver miss PCs.

### 6.1 Dead cells / isolated cells (the big one)

Every empty cell inside the playfield must eventually be covered by a piece
before its row can clear. If an empty cell can no longer be covered by any
legal placement of any still-available piece, the position is dead.

`tetra-tools` implements a very fast special case, `has_isolated_cell`, using
bit magic. On a 4-row board:

```rust
// full[c] = column c is completely filled
// not_empty[c] = column c has at least one cell
let full      = (b >> 30) & (b >> 20) & (b >> 10) & b;
let not_empty = (b >> 30) | (b >> 20) | (b >> 10) | b;

// A cell is bounded if it is full, or the cells left and right are full
// (walls count as filled). Bits wrap at the edge, which is harmless.
let left  = (b << 1) | 0b0000000001_0000000001_0000000001_0000000001;
let right = (b >> 1) | 0b1000000000_1000000000_1000000000_1000000000;
let bounded_cells = (left & right) | b;
let bounded = (bounded_cells >> 30) & (bounded_cells >> 20)
            & (bounded_cells >> 10) & bounded_cells;

// A column that is non-empty, not full, and whose empty cells are all
// left- and right-bounded can only be filled by a vertical I, which is
// impossible because the column already has a cell.
(not_empty & !full & bounded) != 0
```

The reasoning: if an empty cell has filled cells (or walls) on both sides,
only a vertical piece can cover it, and the only vertical tetromino is the
4-long I. If the column already contains a filled cell, the I cannot pass
through it, so the cell can never be filled.

For general boards you can implement the more expensive but equally sound
version: generate all legal placements of all remaining piece types and mark
the cells they cover. Any empty cell left uncovered is dead. In Fusion this
single check sped the ILSZ 6-piece search from **2.4 s to 92 ms (~26×)**.

> Soundness caveat: this check is exactly sound in the *broken-board* model
> (§6.4) because line clears do not move cells. In a model where cleared rows
> physically fall, a cell that is uncoverable now might become coverable after
> a clear, so the check can, in principle, prune a real solution. In practice
> the cells it catches are locally walled-in and cannot be rescued by clears.
> If absolute completeness matters, either use the broken-board model or only
> flag cells that are uncoverable *ignoring support* (which is invariant under
> clears).

### 6.2 Imbalanced disconnected regions

A piece is connected, so it can never span two empty regions that do not
touch. Therefore each connected empty region must be tiled by whole
tetrominoes, and its size must be a multiple of 4. If a region has a size
that is not a multiple of 4, the position is dead.

`tetra-tools` detects the common case cheaply with `has_imbalanced_split`:
scan adjacent column pairs; if every row has a filled cell in the two
columns, then everything left of the pair is permanently separated from
everything right of it. If the left side has a filled-cell count that is not
a multiple of 4, it is unfillable. (Since the total per section is a multiple
of 4, `filled % 4 != 0` is equivalent to `empty % 4 != 0`.)

The same caveat as above applies in a falling-rows model: a clear can merge
regions. Use it as a sound prune only in the broken-board model.

### 6.3 Cell-count parity

Each piece adds 4 cells; each cleared line removes 10. If `filled` cells are
on the board and `k` pieces are placed with `L` lines cleared:

```
filled + 4k = 10L
```

At a node with `remaining` pieces left, the solver needs some `k <= remaining`
and `L >= 0` satisfying this. A weak but free check is that
`filled + 4k` must be divisible by 10 for some small `k`. Stronger bounds come
from combining this with the minimum number of lines needed to clear the
tallest cells and the maximum cells `remaining` pieces can add. These bounds
are cheap but rarely prune much on their own.

### 6.4 The broken-board model

To make §6.1 and §6.2 exactly sound, `tetra-tools` keeps **cleared lines in
place** instead of letting rows fall. A `BrokenBoard` stores:

* the physical board,
* a bitmask of which rows are cleared,
* the list of placed pieces, each of which may be "broken" across a cleared
  row.

Because cleared rows stay put, cells never shift, so "this cell is walled in
forever" is a true statement. The solver then searches over broken boards;
when all four rows are marked cleared, the physical board clears. This also
makes board identity independent of the order in which lines cleared, which
is what makes the scan/cull/place BFS cache so effective.

---

## 7. Precomputed legal-board databases

`gomen` (in `wirelyre/tetra-tools`) ships `legal-boards.leb128`: the set of
*all* 4-row boards from which a PC is possible with up to 10 pieces (at most
44 cells). During the scan, any board not in this set is discarded
immediately. This turns the search from "explore the tree" into "intersect
with a known set", which is why gomen is near-instant for 4-line PCs.

The database is built by the companion `legal-boards` crate. The file format
is simple: a LEB128 count followed by LEB128 deltas between sorted board
values.

Costs and limits:

* The database is only valid for the board height and ruleset it was built
  for (here, 4 rows, SRS/TETR.IO physics).
* It grows quickly with allowed piece count and board height.
* For taller boards or different rulesets you must regenerate it.

If you only need 4-line PCs, this is by far the fastest approach. For general
downstack PCs, a well-pruned DFS is more flexible.

---

## 8. Move ordering

With a time limit, the order in which children are searched decides whether
you find a solution at all. Good PC ordering signals, in roughly descending
importance:

1. **Line clears** — a move that clears a line is usually progress.
2. **Covers the lowest empty cell** — PC play fills from the bottom up. Bonus
   moves whose cells include the lowest empty cell.
3. **Fewer holes** — strongly penalize creating new holes.
4. **Lower resulting height** — keep the stack flat.
5. **Bumpiness / surface smoothness** — penalize jagged skylines.

A concrete score used by Fusion:

```rust
score = clears * 10_000
      + (covers_lowest ? 4_000 : 0)
      - holes * 200
      - height * 20;
```

Sort children by descending score. Combined with iterative deepening and a
TT, this is often the difference between "never finds it" and "finds it in
milliseconds".

A stronger, more expensive option is a **transposition-table move** (the move
that solved a related state before) or a **neural network** ordering (below).

---

## 9. Neural-network-guided search

`TemariVirus/perfect-tetris` trains a small neural network to score candidate
placements, then uses it to order (and prune) the search. Reported numbers
are on the order of 10 ms mean / 240 ms worst case for 4-line and 6-line PCs
over 200 random first-PC sequences. This is competitive with the
precomputed-database approach while remaining general.

A practical hybrid:

* Use a precomputed database or the dead-cell/region prunes for correctness.
* Use a learned value network purely for **move ordering**.
* Keep a hard time budget and return the best solution found so far.

---

## 10. Hold handling

The hold slot adds a second way to choose the active piece. Model it exactly:

* **Empty hold**: swapping places the next queue piece on the board, puts the
  current piece into hold, and consumes one extra queue entry.
* **Non-empty hold**: swapping exchanges the current piece with the held
  piece; the held piece becomes active and the current goes to hold.

Include the hold state in the TT key. In iterative deepening, treat a hold
swap as a zero-piece action (it changes the state without placing a piece), or
fold it into the placement as Fusion does: generate the same placement for
both the current and the held piece, with the appropriate next state.

---

## 11. A practical recipe

If you are building a solver today, this order gets the best return:

1. Bitboard board + column bitboards.
2. Straight-drop move generation; add kicks only if your ruleset needs them.
3. Canonical placements.
4. Iterative-deepening DFS with a depth-aware transposition table.
5. Move ordering: clears, lowest-empty, holes, height.
6. Dead-cell prune (general coverability) — the single biggest win.
7. If you only care about 4-line PCs: build/load a legal-board database and
   use scan/cull/place.
8. If you need more speed: region-size prune, then a neural ordering network.
9. Always enforce a wall-clock budget with periodic checks (every 1024 nodes),
   and return the best solution found so far.

---

## 12. References

* `wirelyre/tetra-tools` — `gomen` (BFS scan/cull/place) and `legal-boards`
  (all 10-piece 4-line PCs). Source of the `has_isolated_cell` and
  `has_imbalanced_split` bit tricks and the broken-board representation.
  <https://github.com/wirelyre/tetra-tools>
* `TemariVirus/perfect-tetris` — neural-network-guided PC solver in Zig with
  published benchmarks.
  <https://github.com/TemariVirus/perfect-tetris>
* `bryanmylee/perfect-clear` — a discussion of real-time PC search, state
  machines, and why naive memoization struggles.
  <https://github.com/bryanmylee/perfect-clear>
* `solution-finder` — command-line Tetris search (PC probability, spin
  searches).
  <https://solution-finder.readthedocs.io/en/latest/>
* Hard Drop Tetris Wiki, "Perfect clear".
  <https://harddrop.com/wiki/Perfect_clear>

---

## 13. Notes on the Fusion implementation

Fusion's PC solver lives in `src/search.rs`:

* `find_best_move_pc` runs iterative deepening over the queue, sharing a
  `HashMap<u64, u8>` transposition table keyed by board, active piece, hold,
  and queue index.
* `pc_dfs` generates candidates for the current and held pieces, scores and
  sorts them (`pc_score`), and recurses.
* `pc_has_dead_cell` performs the general dead-cell prune: it marks every cell
  coverable by any legal placement of any still-available piece and rejects
  positions with an uncoverable empty cell. It is only run when the board has
  holes, since it costs a round of move generation.
* `PC_MAX_EXTRA_HEIGHT` bounds how far above the starting height the search may
  stack, keeping it out of hopeless tall subtrees.

Measured effect on the ILSZ 6-piece opening (queue `ILSZT`): ~2.4 s before the
rewrite, ~92 ms after. The 4-wide well (height 6) case is solved in ~300 ms.
