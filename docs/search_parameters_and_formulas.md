# Fusion Search Parameters and Evaluation Formulas

This document provides a technical reference for the search parameters and the mathematical formulas used by the Fusion engine to evaluate Tetris positions.

## Search Parameters

These parameters control the behavior of the beam search algorithm. They can be passed in the `search` object of an API request.

| Parameter | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `beam_width` | `usize` | `800` | Number of states kept per expansion layer. Higher values improve tactical quality but increase latency. |
| `depth` | `usize` | `14` | Nominal search horizon (number of pieces ahead). |
| `futility_delta` | `f32` | `15.0` | Pruning threshold. Nodes with scores more than `futility_delta` below the best node at a given layer are pruned. |
| `time_budget_ms` | `u64` | `None` | Soft time limit for the search. If set, the search will iteratively increase width until the budget is reached. |
| `use_tt` | `bool` | `false` | Whether to use a Transposition Table to cache and reuse evaluations for identical board states. |
| `extend_queue_7bag` | `bool` | `true` | If true, the engine assumes a 7-bag randomizer to extend a short piece queue. |
| `attack_weight` | `f32` | `0.50` | Weight for the offensive output (garbage sent). |
| `chain_weight` | `f32` | `0.15` | Weight for combo progression. |
| `b2b_weight` | `f32` | `0.20` | Weight for Back-to-Back (B2B) status. |
| `downstack_weight` | `f32` | `0.20` | Global weight for the downstack component. |
| `downstack_avg_height_weight` | `f32` | `0.0` | Weight for the average column height within the downstack component. |
| `downstack_min_height_weight` | `f32` | `-5.0` | Weight for the minimum column height within the downstack component. |
| `pc_mode` | `bool` | `false` | If true, the engine searches specifically for a Perfect Clear sequence. |
| `context_weight` | `f32` | `0.25` | Weight for situational context (pressure, timing, coaching obligations). |
| `board_weight` | `f32` | `1.0` | Weight for the structural board quality. |
| `path_decay` | `f32` | `1.0` | Exponential decay factor for cumulative path scores. Higher values (closer to 1.0) preserve more influence from earlier moves. |
| `max_depth_factor` | `f32` | `2.45` | Capping factor for depth-based normalization of cumulative scores. |
| `quiescence_max_extensions` | `usize` | `3` | Maximum number of extra moves to search in "loud" (unstable) positions (e.g., mid-combo). |
| `quiescence_beam_fraction` | `f32` | `0.15` | Fraction of the `beam_width` used during quiescence extensions. |

---

## Evaluation Formulas

### 1. Composite Score

The total score $S$ for a search node at depth $d$ is a weighted sum of several components:

$$S = S_{\text{board}} \cdot W_{\text{board}} + \frac{A_{\text{path}}}{\text{norm}(d)} \cdot W_{\text{attack}} + \frac{C_{\text{path}}}{\text{norm}(d)} \cdot W_{\text{chain}} + \log_2(B_{\text{next}} + 1) \cdot W_{\text{b2b}} + \frac{D_{\text{path}}}{\text{norm}(d)} \cdot W_{\text{downstack}} + \frac{M_{\text{path}}}{\text{norm}(d)} \cdot W_{\text{context}} - P_{\text{b2b\_break}}$$

Where:
- $S_{\text{board}}$: Structural board evaluation.
- $A_{\text{path}}, C_{\text{path}}, D_{\text{path}}, M_{\text{path}}$: Exponentially decaying cumulative values for attack, chain, downstack, and context.
- For any path metric $V_{\text{path}}$ and move value $v_i$ at depth $i$:
  $V_{\text{path}, d} = v_d + k \cdot V_{\text{path}, d-1} = \sum_{i=1}^{d} v_i \cdot k^{d-i}$
  where $k$ is the `path_decay` parameter ($0 \le k \le 1$).
- $B_{\text{next}}$: Back-to-Back level after the move. (Logarithmic scaling incentivizes growth while discouraging breaking).
- $P_{\text{b2b\_break}}$: Penalty applied if a B2B chain is broken ($B_{\text{prev}} > 0$ and $B_{\text{next}} = 0$ on a line clear).
- $\text{norm}(d) = \min(\sqrt{d+1}, \text{max\_depth\_factor})$: Normalization factor.

---

## Perfect Clear (PC) Mode

When `pc_mode` is enabled, the engine switches to a specialized depth-first search algorithm optimized for finding Perfect Clears.

### Constraints:
1. **Board Height**: The search is pruned immediately if any move results in a board height exceeding **6 lines**.
2. **Goal-Oriented**: The search terminates and returns the full path as soon as a state with an empty board is reached.
3. **Pruning**: A local transposition table caches visited board states to prevent redundant exploration of identical configurations at the same depth.

If no PC sequence is found within the specified `depth` limit (usually matching the available piece queue), the engine returns no result.

### 2. Board Structural Evaluation

The board score $S_{\text{board}}$ is calculated by summing weighted features:

$$S_{\text{board}} = \sum_{i} w_i \cdot f_i$$

#### Key Features ($f_i$):
- **Holes**: Number of empty cells that have at least one occupied cell above them in the same column.
- **Cell Coveredness**: Number of occupied cells above the topmost hole in each column (capped at 6 per column).
- **Height**: The maximum column height $H_{max}$.
- **Height Penalties**: 
  - If $H_{max} > 10$, add penalty proportional to $(H_{max} - 10)$.
  - If $H_{max} > 15$, add penalty proportional to $(H_{max} - 15)$.
- **Bumpiness**: Sum of absolute differences between adjacent column heights: $\sum |h_j - h_{j+1}|$.
- **Bumpiness Squared**: Sum of squared differences: $\sum (h_j - h_{j+1})^2$.
- **Row Transitions**: Number of occupied-to-empty transitions across all rows (including transitions to/from walls).
- **Well Depth**: Depth of the deepest "well" (a column significantly lower than its immediate neighbors).
- **TSD Overhangs**: Bonus for structural patterns ready for T-Spin Doubles.
- **4-Wide Well**: Bonus for maintaining a side-well suitable for 4-wide combos.

### 3. Chain (Combo) Shaping

Raw combo values are transformed using an exponential saturation function to reward the start of a combo more than the tail end of an extremely long one:

$$V_{\text{chain}}(\text{combo}) = \text{clamp}(1 - e^{-0.25 \cdot \text{combo}}, 0, 1)$$

### 4. Contextual Modifier

The context modifier $M_{\text{context}}$ accounts for tactical urgency and coaching advice:

$$M_{\text{context}} = \text{clamp}(\Delta\text{combo} + \Delta\text{coaching}, -1, 1)$$

The $\Delta\text{coaching}$ is the difference in "safety score" between the next state and the previous state, considering:
- **Fatality**: Penalizes states that are `Critical` or `Fatal`.
- **Obligation**: Penalizes ignoring `MustDownstack` or `MustCancel` requirements.
- **Surge**: Rewards building or activating a surge (aggressive garbage release).
- **Phase**: Small adjustments based on game phase (Opener vs. Endgame).

### 5. Win Probability (Analysis Only)

During replay analysis, Fusion converts raw scores into a Win Probability $P_{wp}$ using a sigmoid function:

$$P_{wp}(S) = \frac{1}{1 + e^{-k(S - c)}}$$

Where:
- $k = 0.1$: Sensitivity constant.
- $c$: Inflection point, dynamically calculated based on the player's skill level (PPS, APP, DSP).

Move severity (Inaccuracy, Mistake, Blunder) is then determined by the drop in Win Probability: $\Delta P_{wp} = P_{wp}(S_{\text{best}}) - P_{wp}(S_{\text{actual}})$.
