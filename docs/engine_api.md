# Fusion Engine API

This document explains how to run and call the HTTP API server added in `engine_api`.

## Overview

The server exposes JSON endpoints for:

1. Finding the best move (`/v1/find_best_move`)
2. Listing all legal moves (`/v1/get_all_moves`)
3. Evaluating a played position (`/v1/evaluate_position`)
4. Converting a move to controller inputs (`/v1/get_input_sequence`)

## Run The Server

Build and run:

```bash
cargo run --release --bin engine_api
```

Default address:

- `127.0.0.1:8787`

Set custom bind address:

```bash
ENGINE_API_ADDR=0.0.0.0:8787 cargo run --release --bin engine_api
```

Health check:

```bash
curl -s http://127.0.0.1:8787/health
```

Expected response:

```json
{"ok":true,"service":"fusion-engine-api"}
```

## Data Encoding

### Piece IDs

- `0 = I`
- `1 = O`
- `2 = T`
- `3 = S`
- `4 = Z`
- `5 = J`
- `6 = L`

### Rotation IDs

- `0 = North`
- `1 = East`
- `2 = South`
- `3 = West`

### Spin IDs

- `0 = NoSpin`
- `1 = Mini`
- `2 = Full`

### Board Format

`board_rows` is an array of row bitmasks from bottom to top.

- At most 40 rows
- Each row uses the lower 10 bits (`0..9`) for occupied cells

Example row value:

- `0b0000001111` means columns `0..3` occupied on that row

## Endpoint: GET /health

Simple liveness probe.

### Response

```json
{
  "ok": true,
  "service": "fusion-engine-api"
}
```

## Endpoint: POST /v1/find_best_move

Finds best move from a position.

### Request

```json
{
  "board_rows": [],
  "current_piece": 2,
  "queue": [0, 1, 6],
  "hold": null,
  "b2b": 0,
  "combo": 0,
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

All fields except `board_rows` and `current_piece` are optional.

### Response

```json
{
  "best_move": {
    "piece": 0,
    "rotation": 0,
    "x": 1,
    "y": 0,
    "spin": 0,
    "hold_used": true,
    "score": 1.1999998,
    "probability": null
  },
  "score": 1.1999998,
  "hold_used": true,
  "pv": [
    {
      "piece": 0,
      "rotation": 0,
      "x": 1,
      "y": 0,
      "spin": 0,
      "hold_used": false,
      "score": null,
      "probability": null
    }
  ],
  "candidates": [
    {
      "piece": 0,
      "rotation": 0,
      "x": 1,
      "y": 0,
      "spin": 0,
      "hold_used": false,
      "score": 1.1999998,
      "probability": 0.14014758
    }
  ]
}
```

`candidates` is only present when `include_candidates=true`.

## Endpoint: POST /v1/get_all_moves

Returns all legal placements for a piece on the board.

### Request

```json
{
  "board_rows": [],
  "current_piece": 2
}
```

### Response

```json
{
  "moves": [
    {
      "piece": 2,
      "rotation": 0,
      "x": 4,
      "y": 0,
      "spin": 0,
      "hold_used": false,
      "score": null,
      "probability": null
    }
  ]
}
```

## Endpoint: POST /v1/evaluate_position

Compares an actual move outcome (`post_board_rows`) against best move from `pre_board_rows`.

### Request

```json
{
  "pre_board_rows": [],
  "post_board_rows": [],
  "current_piece": 2,
  "frame": {
    "queue": [0, 1, 6],
    "hold": null,
    "player_pps": 1.57,
    "player_app": 0.48,
    "player_dsp": 0.2,
    "lines_cleared": 0,
    "b2b": 0,
    "combo": 0,
    "combo_before": 0,
    "hold_used": false,
    "pending_garbage": 0,
    "imminent_garbage": 0
  },
  "search": {
    "beam_width": 800,
    "depth": 14
  }
}
```

`frame` and `search` are optional.

### Response

```json
{
  "eval_before": 0.0,
  "eval_after": 0.0,
  "best_eval": 1.23,
  "best_move": {
    "piece": 2,
    "rotation": 1,
    "x": 4,
    "y": 1,
    "spin": 0,
    "hold_used": false,
    "score": 1.23,
    "probability": null
  },
  "eval_loss": 0.2,
  "severity": "inaccuracy",
  "meter_value": 0.0,
  "position_complexity": 0.13,
  "board_score": 0.4,
  "attack_score": 0.5,
  "chain_score": 0.2,
  "context_score": 0.1,
  "path_attack": 1.1,
  "path_chain": 0.4,
  "path_context": 0.2,
  "recommended_path": [],
  "insight_tags": ["attack_window_miss"]
}
```

## Endpoint: POST /v1/get_input_sequence

Converts a chosen move into control inputs.

### Request

```json
{
  "board_rows": [],
  "mv": {
    "piece": 2,
    "rotation": 0,
    "x": 4,
    "y": 0,
    "spin": 0
  },
  "use_finesse": false,
  "force": false
}
```

### Response

```json
{
  "inputs": [5, 2, 9],
  "input_count": 3
}
```

### Input Code Mapping

- `0 = NoInput`
- `1 = ShiftLeft`
- `2 = ShiftRight`
- `3 = DasLeft`
- `4 = DasRight`
- `5 = RotateCw`
- `6 = RotateCcw`
- `7 = RotateFlip`
- `8 = SoftDrop`
- `9 = HardDrop`

## JavaScript Example

```js
const base = "http://127.0.0.1:8787";

const res = await fetch(`${base}/v1/find_best_move`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({
    board_rows: [],
    current_piece: 2,
    queue: [0, 1, 6],
    include_candidates: true
  })
});

if (!res.ok) {
  const err = await res.json();
  throw new Error(err.error || "request failed");
}

const data = await res.json();
console.log("best move", data.best_move);
```

## Error Format

All error responses use:

```json
{
  "error": "...message..."
}
```
