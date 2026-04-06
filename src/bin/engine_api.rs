use axum::http::Method;
use axum::routing::{get, post};
use axum::{Json, Router};
use direct_cobra_copy::analysis::{
    self, detect_insights, normalize_meter, InsightDetectorInput, PlayerSkill, SIGMOID_K,
};
use direct_cobra_copy::board::Board;
use direct_cobra_copy::eval::{evaluate, EvalWeights};
use direct_cobra_copy::header::{Move, Piece, Rotation, SpinType};
use direct_cobra_copy::movegen::generate;
use direct_cobra_copy::pathfinder;
use direct_cobra_copy::search::{
    find_best_move_with_scores, find_best_move_with_scores_forced, SearchConfig,
};
use direct_cobra_copy::state::GameState;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    service: &'static str,
}

#[derive(Deserialize)]
struct SearchRequest {
    board_rows: Vec<u64>,
    current_piece: u8,
    queue: Option<Vec<u8>>,
    hold: Option<u8>,
    b2b: Option<u8>,
    combo: Option<u32>,
    pending_garbage: Option<u8>,
    search: Option<SearchOverrides>,
    include_candidates: Option<bool>,
    candidate_limit: Option<usize>,
    candidate_temperature: Option<f32>,
}

#[derive(Deserialize)]
struct SearchOverrides {
    beam_width: Option<usize>,
    depth: Option<usize>,
    futility_delta: Option<f32>,
    time_budget_ms: Option<u64>,
    use_tt: Option<bool>,
    extend_queue_7bag: Option<bool>,
    attack_weight: Option<f32>,
    chain_weight: Option<f32>,
    context_weight: Option<f32>,
    board_weight: Option<f32>,
    quiescence_max_extensions: Option<usize>,
    quiescence_beam_fraction: Option<f32>,
}

#[derive(Deserialize)]
struct AllMovesRequest {
    board_rows: Vec<u64>,
    current_piece: u8,
}

#[derive(Deserialize)]
struct EvaluatePositionRequest {
    pre_board_rows: Vec<u64>,
    post_board_rows: Vec<u64>,
    current_piece: u8,
    frame: Option<ReplayFrameContext>,
    search: Option<SearchOverrides>,
}

#[derive(Deserialize)]
struct ReplayFrameContext {
    queue: Option<Vec<u8>>,
    hold: Option<u8>,
    player_pps: Option<f32>,
    player_app: Option<f32>,
    player_dsp: Option<f32>,
    lines_cleared: Option<u8>,
    b2b: Option<i32>,
    combo: Option<i32>,
    combo_before: Option<i32>,
    hold_used: Option<bool>,
    pending_garbage: Option<u32>,
    imminent_garbage: Option<u32>,
}

#[derive(Deserialize)]
struct InputSequenceRequest {
    board_rows: Vec<u64>,
    mv: MoveJson,
    use_finesse: Option<bool>,
    force: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone)]
struct MoveJson {
    piece: u8,
    rotation: u8,
    x: i8,
    y: i8,
    spin: u8,
    hold_used: Option<bool>,
    score: Option<f32>,
    probability: Option<f32>,
}

#[derive(Serialize)]
struct SearchResponse {
    best_move: MoveJson,
    score: f32,
    hold_used: bool,
    pv: Vec<MoveJson>,
    candidates: Option<Vec<MoveJson>>,
}

#[derive(Serialize)]
struct AllMovesResponse {
    moves: Vec<MoveJson>,
}

#[derive(Serialize)]
struct PositionEvalResponse {
    eval_before: f32,
    eval_after: f32,
    best_eval: f32,
    best_move: MoveJson,
    eval_loss: f32,
    severity: String,
    meter_value: f32,
    position_complexity: f32,
    board_score: f32,
    attack_score: f32,
    chain_score: f32,
    context_score: f32,
    path_attack: f32,
    path_chain: f32,
    path_context: f32,
    recommended_path: Vec<MoveJson>,
    insight_tags: Vec<String>,
}

#[derive(Serialize)]
struct InputSequenceResponse {
    inputs: Vec<u8>,
    input_count: usize,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/find_best_move", post(find_best_move_handler))
        .route("/v1/get_all_moves", post(get_all_moves_handler))
        .route("/v1/evaluate_position", post(evaluate_position_handler))
        .route("/v1/get_input_sequence", post(get_input_sequence_handler))
        .layer(cors);

    let addr = std::env::var("ENGINE_API_ADDR")
        .ok()
        .and_then(|s| s.parse::<SocketAddr>().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8787)));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {}: {}", addr, e));

    println!("engine_api listening on http://{}", addr);
    axum::serve(listener, app)
        .await
        .unwrap_or_else(|e| panic!("server error: {}", e));
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "fusion-engine-api",
    })
}

async fn find_best_move_handler(
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, Json<ErrorResponse>> {
    let board = board_from_rows(&req.board_rows)?;
    let current_piece = piece_from_external(req.current_piece)?;
    let queue = queue_from_external(req.queue.as_deref())?;
    let hold = hold_from_external(req.hold)?;

    let mut game_state = GameState::new(board, current_piece, queue);
    game_state.hold = hold;
    game_state.b2b = req.b2b.unwrap_or(0);
    game_state.combo = req.combo.unwrap_or(0);
    game_state.pending_garbage = req.pending_garbage.unwrap_or(0);

    let config = apply_search_overrides(req.search.as_ref());
    let weights = EvalWeights::default();

    let full = find_best_move_with_scores(&game_state, &config, &weights).ok_or_else(|| {
        Json(ErrorResponse {
            error: "no legal move found".to_string(),
        })
    })?;

    let best = &full.best;
    let include_candidates = req.include_candidates.unwrap_or(false);
    let candidates = if include_candidates {
        let temperature = req.candidate_temperature.unwrap_or(1.0).max(0.001);
        let limit = req
            .candidate_limit
            .unwrap_or(8)
            .max(1)
            .min(full.root_scores.len().max(1));
        Some(root_scores_to_candidates(
            &full.root_scores,
            temperature,
            limit,
            &game_state.board,
        ))
    } else {
        None
    };

    let normalized_best = normalize_root_move_for_reachability(&game_state.board, best.best_move);
    let pv = best
        .pv
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let mv = if idx == 0 {
                normalize_root_move_for_reachability(&game_state.board, *m)
            } else {
                *m
            };
            move_to_json(mv, false, None, None)
        })
        .collect();

    let response = SearchResponse {
        best_move: move_to_json(normalized_best, best.hold_used, Some(best.score), None),
        score: best.score,
        hold_used: best.hold_used,
        pv,
        candidates,
    };

    Ok(Json(response))
}

async fn get_all_moves_handler(
    Json(req): Json<AllMovesRequest>,
) -> Result<Json<AllMovesResponse>, Json<ErrorResponse>> {
    let board = board_from_rows(&req.board_rows)?;
    let piece = piece_from_external(req.current_piece)?;

    let mut buffer = direct_cobra_copy::move_buffer::MoveBuffer::new();
    generate(&board, &mut buffer, piece, false);

    let moves = buffer
        .as_slice()
        .iter()
        .map(|m| {
            let normalized = normalize_root_move_for_reachability(&board, *m);
            move_to_json(normalized, false, None, None)
        })
        .collect();

    Ok(Json(AllMovesResponse { moves }))
}

async fn evaluate_position_handler(
    Json(req): Json<EvaluatePositionRequest>,
) -> Result<Json<PositionEvalResponse>, Json<ErrorResponse>> {
    let pre_board = board_from_rows(&req.pre_board_rows)?;
    let post_board = board_from_rows(&req.post_board_rows)?;
    let current_piece = piece_from_external(req.current_piece)?;

    let frame = req.frame.unwrap_or(ReplayFrameContext {
        queue: None,
        hold: None,
        player_pps: None,
        player_app: None,
        player_dsp: None,
        lines_cleared: None,
        b2b: None,
        combo: None,
        combo_before: None,
        hold_used: None,
        pending_garbage: None,
        imminent_garbage: None,
    });

    let queue = queue_from_external(frame.queue.as_deref())?;
    let hold = hold_from_external(frame.hold)?;

    let mut game_state = GameState::new(pre_board.clone(), current_piece, queue);
    game_state.hold = hold;
    game_state.b2b = frame.b2b.unwrap_or(0).max(0) as u8;
    game_state.combo = frame.combo_before.unwrap_or(0).max(0) as u32;
    game_state.pending_garbage = frame.pending_garbage.unwrap_or(0) as u8;

    let mut config = apply_search_overrides(req.search.as_ref());
    config.time_budget_ms = None;
    config.attack_config.pc_garbage = 0;
    config.attack_config.pc_b2b = 0;

    let weights = EvalWeights::default();

    let eval_before = evaluate(&game_state.board, &weights);
    let eval_after = evaluate(&post_board, &weights);

    let mut actual_move_for_search: Option<Move> = None;
    let mut actual_move_raw: Option<u16> = None;
    {
        let mut moves = direct_cobra_copy::move_buffer::MoveBuffer::new();
        generate(&pre_board, &mut moves, current_piece, false);
        for m in moves.as_slice() {
            let mut trial = pre_board.clone();
            trial.do_move(m);
            if trial.rows == post_board.rows {
                actual_move_for_search = Some(*m);
                actual_move_raw = Some(m.raw());
                break;
            }
        }
    }

    let full_result =
        find_best_move_with_scores_forced(&game_state, &config, &weights, actual_move_for_search)
            .ok_or_else(|| {
                Json(ErrorResponse {
                    error: "no legal move found".to_string(),
                })
            })?;

    let sr = &full_result.best;
    let best_score = sr.score;
    let actual_search_score = actual_move_raw.and_then(|raw| {
        let hu = frame.hold_used.unwrap_or_else(|| {
            game_state.infer_hold_used_for_piece(actual_move_for_search.unwrap().piece())
        });
        full_result
            .root_scores
            .iter()
            .find(|(m, root_hu, _)| m.raw() == raw && *root_hu == hu)
            .map(|(_, _, score)| *score)
    });

    let (eval_loss, severity) = if let Some(actual_score) = actual_search_score {
        let raw_loss = (best_score - actual_score).max(0.0);
        let amplified_actual = best_score - raw_loss;

        let skill = PlayerSkill {
            pps: frame.player_pps.unwrap_or(1.57),
            app: frame.player_app.unwrap_or(0.48),
            dsp: frame.player_dsp.unwrap_or(0.20),
        };
        let sigmoid_c = analysis::compute_sigmoid_c(&skill);
        (
            raw_loss,
            analysis::classify_win_prob_drop(best_score, amplified_actual, SIGMOID_K, sigmoid_c),
        )
    } else {
        (0.0, analysis::Severity::None)
    };

    let combo_after = frame.combo.unwrap_or(0).max(0) as u32;
    let combo_before = frame.combo_before.unwrap_or(0).max(0) as u32;
    let lines_cleared_val = frame.lines_cleared.unwrap_or(0);

    let insight_input = InsightDetectorInput {
        best_attack_score: full_result.path_attack,
        best_chain_score: full_result.path_chain,
        best_board_score: full_result.board_score,
        actual_score: actual_search_score,
        best_score,
        actual_combo_after: combo_after,
        actual_combo_before: combo_before,
        actual_lines_cleared: lines_cleared_val,
        board_eval_delta: eval_after - eval_before,
    };

    let insight_tags = detect_insights(&insight_input)
        .iter()
        .map(|r| r.tag.to_str().to_string())
        .collect::<Vec<_>>();

    let response = PositionEvalResponse {
        eval_before,
        eval_after,
        best_eval: best_score,
        best_move: move_to_json(
            normalize_root_move_for_reachability(&game_state.board, sr.best_move),
            sr.hold_used,
            Some(sr.score),
            None,
        ),
        eval_loss,
        severity: severity_to_string(severity).to_string(),
        meter_value: normalize_meter(eval_after),
        position_complexity: full_result.position_complexity,
        board_score: full_result.board_score,
        attack_score: full_result.attack_score,
        chain_score: full_result.chain_score,
        context_score: full_result.context_score,
        path_attack: full_result.path_attack,
        path_chain: full_result.path_chain,
        path_context: full_result.path_context,
        recommended_path: sr
            .pv
            .iter()
            .enumerate()
            .map(|(idx, m)| {
                let mv = if idx == 0 {
                    normalize_root_move_for_reachability(&game_state.board, *m)
                } else {
                    *m
                };
                move_to_json(mv, false, None, None)
            })
            .collect(),
        insight_tags,
    };

    Ok(Json(response))
}

async fn get_input_sequence_handler(
    Json(req): Json<InputSequenceRequest>,
) -> Result<Json<InputSequenceResponse>, Json<ErrorResponse>> {
    let board = board_from_rows(&req.board_rows)?;
    let target_move = move_from_json(&req.mv)?;

    let inputs = pathfinder::get_input(
        &board,
        &target_move,
        req.use_finesse.unwrap_or(false),
        req.force.unwrap_or(false),
    );

    let response = InputSequenceResponse {
        input_count: inputs.size(),
        inputs: inputs.as_u8_vec(),
    };

    Ok(Json(response))
}

fn apply_search_overrides(overrides: Option<&SearchOverrides>) -> SearchConfig {
    let mut out = SearchConfig::default();
    if let Some(v) = overrides {
        if let Some(x) = v.beam_width {
            out.beam_width = x;
        }
        if let Some(x) = v.depth {
            out.depth = x;
        }
        if let Some(x) = v.futility_delta {
            out.futility_delta = x;
        }
        if let Some(x) = v.time_budget_ms {
            out.time_budget_ms = Some(x);
        }
        if let Some(x) = v.use_tt {
            out.use_tt = x;
        }
        if let Some(x) = v.extend_queue_7bag {
            out.extend_queue_7bag = x;
        }
        if let Some(x) = v.attack_weight {
            out.attack_weight = x;
        }
        if let Some(x) = v.chain_weight {
            out.chain_weight = x;
        }
        if let Some(x) = v.context_weight {
            out.context_weight = x;
        }
        if let Some(x) = v.board_weight {
            out.board_weight = x;
        }
        if let Some(x) = v.quiescence_max_extensions {
            out.quiescence_max_extensions = x;
        }
        if let Some(x) = v.quiescence_beam_fraction {
            out.quiescence_beam_fraction = x;
        }
    }
    out
}

fn board_from_rows(rows: &[u64]) -> Result<Board, Json<ErrorResponse>> {
    if rows.len() > 40 {
        return Err(err("board_rows can have at most 40 rows"));
    }

    let mut board = Board::new();
    for (y, row) in rows.iter().enumerate() {
        board.rows[y] = (row & 0x03FF) as u16;
    }

    board.cols = [0; 10];
    for y in 0..40 {
        let row = board.rows[y];
        if row == 0 {
            continue;
        }
        let mut bits = row as u64;
        while bits != 0 {
            let x = bits.trailing_zeros() as usize;
            board.cols[x] |= 1u64 << y;
            bits &= bits - 1;
        }
    }

    Ok(board)
}

fn piece_from_external(v: u8) -> Result<Piece, Json<ErrorResponse>> {
    match v {
        0 => Ok(Piece::I),
        1 => Ok(Piece::O),
        2 => Ok(Piece::T),
        3 => Ok(Piece::S),
        4 => Ok(Piece::Z),
        5 => Ok(Piece::J),
        6 => Ok(Piece::L),
        _ => Err(err("invalid piece id; expected 0..=6")),
    }
}

fn piece_to_external(p: Piece) -> u8 {
    match p {
        Piece::I => 0,
        Piece::O => 1,
        Piece::T => 2,
        Piece::S => 3,
        Piece::Z => 4,
        Piece::J => 5,
        Piece::L => 6,
    }
}

fn queue_from_external(queue: Option<&[u8]>) -> Result<Vec<Piece>, Json<ErrorResponse>> {
    let mut out = Vec::new();
    if let Some(values) = queue {
        for &v in values {
            out.push(piece_from_external(v)?);
        }
    }
    Ok(out)
}

fn hold_from_external(hold: Option<u8>) -> Result<Option<Piece>, Json<ErrorResponse>> {
    match hold {
        Some(v) => Ok(Some(piece_from_external(v)?)),
        None => Ok(None),
    }
}

fn rotation_from_u8(v: u8) -> Result<Rotation, Json<ErrorResponse>> {
    match v {
        0 => Ok(Rotation::North),
        1 => Ok(Rotation::East),
        2 => Ok(Rotation::South),
        3 => Ok(Rotation::West),
        _ => Err(err("invalid rotation; expected 0..=3")),
    }
}

fn spin_from_u8(v: u8) -> Result<SpinType, Json<ErrorResponse>> {
    match v {
        0 => Ok(SpinType::NoSpin),
        1 => Ok(SpinType::Mini),
        2 => Ok(SpinType::Full),
        _ => Err(err("invalid spin; expected 0..=2")),
    }
}

fn move_from_json(mv: &MoveJson) -> Result<Move, Json<ErrorResponse>> {
    let piece = piece_from_external(mv.piece)?;
    let rotation = rotation_from_u8(mv.rotation)?;
    let spin = spin_from_u8(mv.spin)?;

    let move_value = match spin {
        SpinType::NoSpin => Move::new(piece, rotation, mv.x as i32, mv.y as i32, false),
        SpinType::Mini if piece == Piece::T => Move::new_tspin(rotation, mv.x as i32, mv.y as i32, false),
        SpinType::Mini => Move::new_allspin_mini(piece, rotation, mv.x as i32, mv.y as i32),
        SpinType::Full => Move::new(piece, rotation, mv.x as i32, mv.y as i32, true),
    };

    Ok(move_value)
}

fn move_to_json(mv: Move, hold_used: bool, score: Option<f32>, probability: Option<f32>) -> MoveJson {
    MoveJson {
        piece: piece_to_external(mv.piece()),
        rotation: mv.rotation() as u8,
        x: mv.x() as i8,
        y: mv.y() as i8,
        spin: mv.spin() as u8,
        hold_used: Some(hold_used),
        score,
        probability,
    }
}

fn normalize_root_move_for_reachability(board: &Board, mv: Move) -> Move {
    if mv.spin() == SpinType::NoSpin {
        return mv;
    }

    let exact = pathfinder::get_input(board, &mv, false, false);
    if exact.size() > 0 {
        return mv;
    }

    let fallback = Move::new(mv.piece(), mv.rotation(), mv.x(), mv.y(), false);
    let fallback_inputs = pathfinder::get_input(board, &fallback, false, false);
    if fallback_inputs.size() > 0 {
        return fallback;
    }

    mv
}

fn root_scores_to_candidates(
    root_scores: &[(Move, bool, f32)],
    temperature: f32,
    limit: usize,
    board: &Board,
) -> Vec<MoveJson> {
    let top = &root_scores[..limit.min(root_scores.len())];
    if top.is_empty() {
        return Vec::new();
    }

    let max_score = top
        .iter()
        .map(|(_, _, score)| *score)
        .fold(f32::NEG_INFINITY, f32::max);

    let mut exps = Vec::with_capacity(top.len());
    let mut sum = 0.0f32;
    for (_, _, score) in top {
        let value = ((score - max_score) / temperature).exp();
        exps.push(value);
        sum += value;
    }

    top.iter()
        .zip(exps.iter())
        .map(|((mv, hu, score), exp)| {
            let probability = if sum > 0.0 { *exp / sum } else { 0.0 };
            move_to_json(
                normalize_root_move_for_reachability(board, *mv),
                *hu,
                Some(*score),
                Some(probability),
            )
        })
        .collect()
}

fn severity_to_string(v: analysis::Severity) -> &'static str {
    match v {
        analysis::Severity::None => "none",
        analysis::Severity::Inaccuracy => "inaccuracy",
        analysis::Severity::Mistake => "mistake",
        analysis::Severity::Blunder => "blunder",
    }
}

fn err(message: &str) -> Json<ErrorResponse> {
    Json(ErrorResponse {
        error: message.to_string(),
    })
}
