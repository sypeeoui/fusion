# Fusion Evaluation Parameter Reference

This document is a practical reference for all evaluation parameters used by the zztetris integration when calling the Fusion HTTP API.

It complements the endpoint contract in `engine_api.md` by explaining meaning, impact, and tuning guidance.

## Scope

This reference covers:

- Top-level request fields sent to `POST /v1/find_best_move`
- Search override fields inside `search`
- zztetris-only controls that affect filtering/playback, but are not sent to Fusion

## Top-Level Request Parameters

These are the fields in the JSON body sent to `POST /v1/find_best_move`.

### board_rows

- Type: `number[]` (u64 rows, using lower 10 bits)
- Required: yes
- Meaning: board occupancy from bottom row to top row.
- Notes:
  - Max 40 rows.
  - Bit `1 << x` means occupied cell in column `x`.

### current_piece

- Type: `number`
- Required: yes
- Range: `0..6`
- Meaning: active piece to place now.
- Mapping:
  - `0=I, 1=O, 2=T, 3=S, 4=Z, 5=J, 6=L`

### queue

- Type: `number[]`
- Required: no
- Meaning: upcoming piece IDs after `current_piece`.
- Behavior if omitted: engine evaluates with whatever queue is available (empty/short horizon).

### hold

- Type: `number | null`
- Required: no
- Meaning: piece currently in hold.

### b2b

- Type: `number`
- Required: no
- Default in API: `0`
- Meaning: current Back-to-Back chain level.
- Important semantics in Fusion:
  - `0` means no chain.
  - Difficult clears (spin or quad) increment this value.
  - Non-difficult line clears reset to `0`.
  - No-line moves preserve the current value.

### combo

- Type: `number`
- Required: no
- Default in API: `0`
- Meaning: current combo counter.
- Fusion semantics:
  - Line clear increments combo.
  - No-line move resets combo to `0`.

### pending_garbage

- Type: `number`
- Required: no
- Default in API: `0`
- Meaning: incoming garbage pressure used for context/urgency.

### include_candidates

- Type: `boolean`
- Required: no
- Default in API: `false`
- Meaning: whether to return additional candidate root moves besides best move.

### candidate_limit

- Type: `number`
- Required: no
- Default in API: `8`
- Meaning: max number of candidate routes returned (top-ranked roots).

### candidate_temperature

- Type: `number`
- Required: no
- Default in API: `1.0`
- Meaning: softmax temperature when converting candidate scores to probabilities.
- Typical effect:
  - Lower (`<1.0`) -> sharper probability concentration on top moves.
  - Higher (`>1.0`) -> flatter distribution.

## Search Override Parameters (`search`)

These are nested inside `search: { ... }`.

### beam_width

- Type: `number`
- Typical default: `800`
- Meaning: number of states kept per expansion layer.
- Higher:
  - Better quality in tactical spots.
  - Slower.

### depth

- Type: `number`
- Typical default: `14`
- Meaning: nominal search horizon.
- Higher:
  - Better long-term planning.
  - Slower and more memory.

### time_budget_ms

- Type: `number`
- Typical default: `50`
- Meaning: soft time cap for search.

### futility_delta

- Type: `number`
- Typical default: `15.0`
- Meaning: pruning aggressiveness threshold.
- Higher values generally prune more aggressively.

### use_tt

- Type: `boolean`
- Typical default: `false`
- Meaning: use transposition table to reuse evaluations of repeated states.

### extend_queue_7bag

- Type: `boolean`
- Typical default: `true`
- Meaning: if queue is short, extend it with 7-bag assumptions.

### attack_weight

- Type: `number`
- Typical default: `0.5`
- Meaning: weight for attack potential in final score.

### chain_weight

- Type: `number`
- Typical default: `0.15`
- Meaning: weight for combo/b2b/chain continuation value.

### context_weight

- Type: `number`
- Typical default: `0.1`
- Meaning: weight for situational context (pressure, timing, etc.).

### board_weight

- Type: `number`
- Typical default: `1.0`
- Meaning: weight for board quality (shape/safety/efficiency).

### quiescence_max_extensions

- Type: `number`
- Typical default: `3`
- Meaning: extra tactical extension depth in unstable positions.

### quiescence_beam_fraction

- Type: `number`
- Typical default: `0.15`
- Meaning: fraction of beam retained during quiescence extensions.

## zztetris Integration Controls (Not Sent As Engine Search Params)

These affect UI behavior in zztetris.

### Reachability Mode

- Values: `strict`, `relaxed`, `off`
- Meaning:
  - `strict`: keep only routes that pass reachability check.
  - `relaxed`: if strict removes all, fallback to unfiltered routes.
  - `off`: skip reachability filtering.

### Route Playback Input/s

- Type: number
- Meaning: playback speed when executing route input sequence.

## Tuning Presets

### Fast Interactive

Use when dragging/painting frequently:

- `beam_width=300..500`
- `depth=8..12`
- `time_budget_ms=20..35`

### Balanced Default

Good quality with responsive UX:

- `beam_width=700..1000`
- `depth=12..16`
- `time_budget_ms=40..80`

### Deep Analysis

When paused and studying a position:

- `beam_width=1200+`
- `depth=18+`
- `time_budget_ms=120..300`

## Common Pitfalls

- Sending stale `combo`/`b2b` values can cause recommendations that look inconsistent with expected chain continuation.
- Overly low `time_budget_ms` plus high `depth` can create unstable route ordering.
- Extreme weight imbalance (for example very high `attack_weight` with tiny `board_weight`) can produce overly greedy suggestions.

## Minimal Example

```json
{
  "board_rows": [],
  "current_piece": 2,
  "queue": [0, 1, 6],
  "hold": null,
  "b2b": 2,
  "combo": 3,
  "pending_garbage": 0,
  "include_candidates": true,
  "candidate_limit": 8,
  "candidate_temperature": 1.0,
  "search": {
    "beam_width": 800,
    "depth": 14,
    "futility_delta": 15.0,
    "time_budget_ms": 50,
    "use_tt": false,
    "extend_queue_7bag": true,
    "attack_weight": 0.5,
    "chain_weight": 1.0,
    "context_weight": 0.1,
    "board_weight": 1.0,
    "quiescence_max_extensions": 3,
    "quiescence_beam_fraction": 0.15
  }
}
```
