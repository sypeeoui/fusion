// search.rs -- beam search with hold for coaching engine
// expands moves breadth-first, pruned to beam_width at each depth

use crate::bag;
use crate::default_ruleset::ACTIVE_RULES;

use crate::eval::EvalWeights;
use crate::pathfinder;

use crate::state::GameState;
use crate::transposition::{get_zobrist_keys, TranspositionTable, DEFAULT_TT_SIZE};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

pub use crate::search_config::{SearchConfig, SearchNode, SearchResult, SearchResultFull};
pub(crate) use crate::search_config::{SearchExpansionContext, SearchIterationParams};
pub(crate) use crate::search_expand::{expand_node, gen_and_eval_root};

/// beam search from game state
/// returns the best move found, or None if no legal moves exist
pub fn find_best_move(
    state: &GameState,
    config: &SearchConfig,
    weights: &EvalWeights,
) -> Option<SearchResult> {
    find_best_move_with_scores(state, config, weights).map(|full| full.best)
}

pub fn find_best_move_with_scores(
    state: &GameState,
    config: &SearchConfig,
    weights: &EvalWeights,
) -> Option<SearchResultFull> {
    find_best_move_with_scores_forced(state, config, weights, None)
}

/// Beam search with optional forced root move.
/// When `forced_root_move` is Some, that move is protected from futility pruning
/// and beam truncation — it always survives to the final beam so its score
/// appears in `root_scores`.
pub fn find_best_move_with_scores_forced(
    state: &GameState,
    config: &SearchConfig,
    weights: &EvalWeights,
    forced_root_move: Option<crate::header::Move>,
) -> Option<SearchResultFull> {
    let search_queue = if config.extend_queue_7bag {
        bag::extend_queue(&state.queue, state.current, state.hold)
    } else {
        state.queue.clone()
    };

    let max_depth = config.depth.min(search_queue.len() + 1);
    if max_depth == 0 {
        return None;
    }

    let zobrist_keys = get_zobrist_keys();
    let mut tt = config
        .use_tt
        .then(|| TranspositionTable::new(DEFAULT_TT_SIZE));

    if config.time_budget_ms.is_none() {
        let mut params = SearchIterationParams {
            state,
            queue: &search_queue,
            config,
            weights,
            max_depth,
            beam_width: config.beam_width,
            zobrist_keys,
            tt: &mut tt,
            forced_root_move,
        };
        return run_beam_search_iteration(&mut params);
    }

    let max_width = config.beam_width;
    if max_width == 0 {
        return None;
    }

    let mut width = 200.min(max_width);
    let mut best_full: Option<SearchResultFull> = None;

    #[cfg(not(target_arch = "wasm32"))]
    let start = Instant::now();
    #[cfg(not(target_arch = "wasm32"))]
    let time_budget = config.time_budget_ms.map(Duration::from_millis);

    #[cfg(target_arch = "wasm32")]
    let mut iteration_count = 0usize;
    #[cfg(target_arch = "wasm32")]
    let max_iterations = config
        .time_budget_ms
        .map(|ms| ms.max(1) as usize)
        .unwrap_or(1);

    loop {
        if let Some(table) = tt.as_mut() {
            table.clear();
        }

        let mut params = SearchIterationParams {
            state,
            queue: &search_queue,
            config,
            weights,
            max_depth,
            beam_width: width,
            zobrist_keys,
            tt: &mut tt,
            forced_root_move,
        };
        if let Some(full) = run_beam_search_iteration(&mut params) {
            let should_replace = best_full
                .as_ref()
                .is_none_or(|prev| compare_results_desc(&full.best, &prev.best).is_lt());

            if should_replace {
                best_full = Some(full);
            }
        }

        if width >= max_width {
            break;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(budget) = time_budget {
                if start.elapsed() >= budget {
                    break;
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            iteration_count += 1;
            if iteration_count >= max_iterations {
                break;
            }
        }

        width = (width * 2).min(max_width);
    }

    best_full
}

fn run_beam_search_iteration(params: &mut SearchIterationParams<'_>) -> Option<SearchResultFull> {
    let mut ctx = SearchExpansionContext {
        config: params.config,
        weights: params.weights,
        remaining_depth: params.max_depth.saturating_sub(1),
        zobrist_keys: params.zobrist_keys,
        tt: params.tt,
    };

    let mut beam = expand_root(params.state, &mut ctx);
    if beam.is_empty() {
        return None;
    }

    apply_futility_pruning(
        &mut beam,
        params.config.futility_delta,
        params.forced_root_move,
    );
    beam.sort_unstable_by(compare_nodes_desc);
    truncate_with_forced(&mut beam, params.beam_width, params.forced_root_move);

    for depth_idx in 0..params.max_depth.saturating_sub(1) {
        let queue_piece = match params.queue.get(depth_idx).copied() {
            Some(p) => p,
            None => break,
        };

        let child_depth = depth_idx + 2;
        ctx.remaining_depth = params.max_depth.saturating_sub(child_depth);

        let mut next_beam: Vec<SearchNode> =
            Vec::with_capacity(params.beam_width.saturating_mul(2));

        for node in &beam {
            let current_piece = queue_piece;

            expand_node(
                node,
                current_piece,
                node.hold,
                false,
                &mut ctx,
                &mut next_beam,
            );

            if let Some(held) = node.hold {
                if held != current_piece {
                    expand_node(
                        node,
                        held,
                        Some(current_piece),
                        true,
                        &mut ctx,
                        &mut next_beam,
                    );
                }
            }
        }

        if next_beam.is_empty() {
            break;
        }

        apply_futility_pruning(
            &mut next_beam,
            params.config.futility_delta,
            params.forced_root_move,
        );
        next_beam.sort_unstable_by(compare_nodes_desc);
        truncate_with_forced(&mut next_beam, params.beam_width, params.forced_root_move);
        beam = next_beam;
    }

    // Quiescence extensions: extend loud nodes past the normal depth boundary
    // so investment moves (mid-combo, active B2B) resolve before evaluation.
    let q_max = params.config.quiescence_max_extensions;
    let q_beam_width =
        ((params.beam_width as f32) * params.config.quiescence_beam_fraction).ceil() as usize;
    if q_max > 0 && q_beam_width > 0 {
        let main_depth = params.max_depth.saturating_sub(1);
        let loud_nodes: Vec<SearchNode> = beam.iter().filter(|n| n.is_loud()).cloned().collect();

        if !loud_nodes.is_empty() {
            let mut q_beam = loud_nodes;
            q_beam.sort_unstable_by(compare_nodes_desc);
            q_beam.truncate(q_beam_width);

            for ext in 0..q_max {
                let q_depth_idx = main_depth + ext;
                let queue_piece = match params.queue.get(q_depth_idx).copied() {
                    Some(p) => p,
                    None => break,
                };

                let child_depth = q_depth_idx + 2;
                ctx.remaining_depth = params
                    .max_depth
                    .saturating_sub(child_depth.min(params.max_depth));

                let mut next_q: Vec<SearchNode> = Vec::with_capacity(q_beam_width * 2);

                for node in &q_beam {
                    expand_node(node, queue_piece, node.hold, false, &mut ctx, &mut next_q);
                    if let Some(held) = node.hold {
                        if held != queue_piece {
                            expand_node(node, held, Some(queue_piece), true, &mut ctx, &mut next_q);
                        }
                    }
                }

                if next_q.is_empty() {
                    break;
                }

                next_q.sort_unstable_by(compare_nodes_desc);
                next_q.truncate(q_beam_width);

                for node in &next_q {
                    if !node.is_loud() {
                        beam.push(node.clone());
                    }
                }

                q_beam = next_q.into_iter().filter(|n| n.is_loud()).collect();
                if q_beam.is_empty() {
                    break;
                }
            }

            beam.extend(q_beam);
            beam.sort_unstable_by(compare_nodes_desc);
        }
    }

    let best = beam.first()?;
    let result = SearchResult {
        best_move: best.root_move,
        hold_used: best.root_hold_used,
        score: best.score,
        pv: best.path.to_vec(),
        coaching_state: best.coaching,
        pv_clear_events: best.path_clear_events.to_vec(),
    };

    let mut root_scores: Vec<(crate::header::Move, bool, f32)> = Vec::new();
    for node in &beam {
        let raw = node.root_move.raw();
        match root_scores.iter_mut().find(|entry| entry.0.raw() == raw) {
            Some(entry) => {
                if node.score > entry.2 {
                    entry.1 = node.root_hold_used;
                    entry.2 = node.score;
                }
            }
            None => root_scores.push((node.root_move, node.root_hold_used, node.score)),
        }
    }
    root_scores.sort_by(|a, b| b.2.total_cmp(&a.2));

    let position_complexity = compute_position_complexity(&root_scores);

    Some(SearchResultFull {
        best: result,
        root_scores,
        position_complexity,
        board_score: best.board_score,
        attack_score: best.attack_score,
        chain_score: best.chain_score,
        context_score: best.context_score,
        path_attack: best.path_attack,
        path_chain: best.path_chain,
        path_context: best.path_context,
    })
}

/// Compute position complexity: variance of top-10 root move scores.
/// High variance = sharp position (clear best moves), low = flat (all moves similar).
fn compute_position_complexity(root_scores: &[(crate::header::Move, bool, f32)]) -> f32 {
    let mut top_n = [0.0f32; 10];
    let count = root_scores.len().min(10);
    for (i, (_, _, s)) in root_scores.iter().take(10).enumerate() {
        top_n[i] = *s;
    }
    if count < 2 {
        return 0.0;
    }
    let scores = &top_n[..count];
    let mean = scores.iter().sum::<f32>() / count as f32;
    let variance = scores.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / count as f32;
    variance
}

/// Truncate beam to `max_size`, but if a forced root move would be truncated,
/// re-insert it by evicting the worst node.
fn truncate_with_forced(
    beam: &mut Vec<SearchNode>,
    max_size: usize,
    forced: Option<crate::header::Move>,
) {
    if beam.len() <= max_size {
        return;
    }

    // Extract forced node before truncation so it can't be lost
    let forced_node = forced.and_then(|fm| {
        let idx = beam.iter().position(|n| n.root_move.raw() == fm.raw());
        idx.map(|i| beam.swap_remove(i))
    });

    beam.truncate(max_size);

    // Re-insert forced node, evicting worst survivor if needed
    if let Some(node) = forced_node {
        let already_present = beam
            .iter()
            .any(|n| n.root_move.raw() == node.root_move.raw());
        if !already_present {
            if beam.len() >= max_size {
                beam.pop(); // evict worst (last after sort)
            }
            beam.push(node);
        }
    }
}

fn apply_futility_pruning(
    nodes: &mut Vec<SearchNode>,
    futility_delta: f32,
    forced: Option<crate::header::Move>,
) {
    if nodes.is_empty() {
        return;
    }

    let delta = futility_delta.max(0.0);
    let best_tier = nodes.iter().map(policy_key).max().unwrap_or((0, 0));

    // Extract forced move node before pruning (if present)
    let forced_node = forced.and_then(|fm| {
        let idx = nodes.iter().position(|n| n.root_move.raw() == fm.raw());
        idx.map(|i| nodes.swap_remove(i))
    });

    nodes.retain(|node| policy_key(node) == best_tier);

    let best_score = nodes
        .iter()
        .map(|node| node.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let cutoff = best_score - delta;

    nodes.retain(|node| node.score >= cutoff);

    // Re-insert forced move node unconditionally (it bypasses futility pruning)
    if let Some(forced_node) = forced_node {
        // Only re-insert if not already present (it might have survived pruning
        // if it was removed by swap_remove but an identical root_move node exists)
        let already_present = nodes
            .iter()
            .any(|n| n.root_move.raw() == forced_node.root_move.raw());
        if !already_present {
            nodes.push(forced_node);
        }
    }
}

fn policy_key(node: &SearchNode) -> (u8, u8) {
    let survival = match node.coaching.fatality {
        crate::state::FatalityState::Fatal => 0,
        crate::state::FatalityState::Critical => 1,
        crate::state::FatalityState::Safe => 2,
    };

    let obligation = match node.coaching.obligation {
        crate::state::ObligationState::MustCancel => 0,
        crate::state::ObligationState::MustDownstack => 1,
        crate::state::ObligationState::None => 2,
    };

    (survival, obligation)
}

fn compare_nodes_desc(a: &SearchNode, b: &SearchNode) -> std::cmp::Ordering {
    let a_key = policy_key(a);
    let b_key = policy_key(b);

    b_key.cmp(&a_key).then_with(|| b.score.total_cmp(&a.score))
}

fn compare_results_desc(a: &SearchResult, b: &SearchResult) -> std::cmp::Ordering {
    let a_survival = match a.coaching_state.fatality {
        crate::state::FatalityState::Fatal => 0,
        crate::state::FatalityState::Critical => 1,
        crate::state::FatalityState::Safe => 2,
    };
    let b_survival = match b.coaching_state.fatality {
        crate::state::FatalityState::Fatal => 0,
        crate::state::FatalityState::Critical => 1,
        crate::state::FatalityState::Safe => 2,
    };

    let a_obligation = match a.coaching_state.obligation {
        crate::state::ObligationState::MustCancel => 0,
        crate::state::ObligationState::MustDownstack => 1,
        crate::state::ObligationState::None => 2,
    };
    let b_obligation = match b.coaching_state.obligation {
        crate::state::ObligationState::MustCancel => 0,
        crate::state::ObligationState::MustDownstack => 1,
        crate::state::ObligationState::None => 2,
    };

    (b_survival, b_obligation)
        .cmp(&(a_survival, a_obligation))
        .then_with(|| b.score.total_cmp(&a.score))
}

fn expand_root(state: &GameState, ctx: &mut SearchExpansionContext<'_>) -> Vec<SearchNode> {
    let mut nodes = Vec::with_capacity(128);

    gen_and_eval_root(state, state.current, state.hold, false, ctx, &mut nodes);

    if let Some(held) = state.hold {
        if held != state.current {
            gen_and_eval_root(state, held, Some(state.current), true, ctx, &mut nodes);
        }
    }

    nodes.retain(|node| !is_root_rotation_artifact(&state.board, node.root_move));

    nodes
}

fn is_root_rotation_artifact(board: &crate::board::Board, mv: crate::header::Move) -> bool {
    let piece = mv.piece();
    if piece != crate::header::Piece::S
        && piece != crate::header::Piece::Z
        && piece != crate::header::Piece::I
    {
        return false;
    }

    let Some(reachable_mv) = pathfinder::normalize_lock_move_for_reachability(board, mv, false)
    else {
        return true;
    };

    if reachable_mv.spin() != crate::header::SpinType::NoSpin {
        return false;
    }

    let normal = pathfinder::get_input(board, &reachable_mv, false, false);
    let forced = pathfinder::get_input(board, &reachable_mv, false, true);

    let normal_suspicious = input_sequence_is_rotation_artifact(&normal, reachable_mv);
    let forced_suspicious = input_sequence_is_rotation_artifact(&forced, reachable_mv);

    let mut available_paths = 0usize;
    let mut suspicious_paths = 0usize;

    if normal.size() > 0 {
        available_paths += 1;
        if normal_suspicious {
            suspicious_paths += 1;
        }
    }

    if forced.size() > 0 {
        available_paths += 1;
        if forced_suspicious {
            suspicious_paths += 1;
        }
    }

    available_paths == 0 || suspicious_paths == available_paths
}

fn minimal_rotation_steps_from_spawn(target_rotation: crate::header::Rotation) -> usize {
    use crate::header::Rotation;

    match target_rotation {
        Rotation::North => 0,
        Rotation::East | Rotation::West => 1,
        Rotation::South => {
            if ACTIVE_RULES.enable_180 {
                1
            } else {
                2
            }
        }
    }
}

fn input_sequence_is_rotation_artifact(
    inputs: &pathfinder::Inputs,
    target_move: crate::header::Move,
) -> bool {
    if inputs.size() == 0 {
        return false;
    }

    let min_rotations = minimal_rotation_steps_from_spawn(target_move.rotation());
    let mut rotation_count = 0usize;
    let mut horizontal_count = 0usize;
    let mut softdrop_count = 0usize;
    let mut rotate_flip_count = 0usize;
    let mut first_softdrop_seen = false;
    let mut rotations_after_first_softdrop = 0usize;

    for input in &inputs.data {
        match input {
            pathfinder::Input::RotateCw | pathfinder::Input::RotateCcw => {
                rotation_count += 1;
                if first_softdrop_seen {
                    rotations_after_first_softdrop += 1;
                }
            }
            pathfinder::Input::RotateFlip => {
                rotation_count += 1;
                rotate_flip_count += 1;
                if first_softdrop_seen {
                    rotations_after_first_softdrop += 1;
                }
            }
            pathfinder::Input::ShiftLeft
            | pathfinder::Input::ShiftRight
            | pathfinder::Input::DasLeft
            | pathfinder::Input::DasRight => horizontal_count += 1,
            pathfinder::Input::SoftDrop => {
                softdrop_count += 1;
                first_softdrop_seen = true;
            }
            _ => {}
        }
    }

    if horizontal_count != 0 || rotation_count == 0 {
        return false;
    }

    let extra_rotations = rotation_count.saturating_sub(min_rotations);

    // Definite root invariant for S/Z/I artifact paths:
    // no horizontal movement plus rotation count far above what's needed to
    // reach the target orientation (especially after softdrop) indicates
    // a pathfinder rotation-loop artifact.
    extra_rotations >= 3
        || (softdrop_count >= 2 && extra_rotations >= 2)
        || (rotations_after_first_softdrop >= 3)
        || (rotate_flip_count > 0 && extra_rotations >= 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bag;
    use crate::board::{Board, FULL_ROW};
    use crate::header::{Move, Piece, Rotation, COL_NB};
    use crate::state::CoachingState;
    use smallvec::{smallvec, SmallVec};
    fn make_node(
        score: f32,
        fatality: crate::state::FatalityState,
        obligation: crate::state::ObligationState,
    ) -> SearchNode {
        let coaching = CoachingState {
            fatality,
            obligation,
            ..CoachingState::default()
        };

        SearchNode {
            board: Board::new(),
            score,
            hold: None,
            b2b: 0,
            combo: 0,
            pending_garbage: 0,
            coaching,
            root_move: Move::none(),
            root_hold_used: false,
            path: smallvec![Move::none()],
            board_score: 0.0,
            attack_score: 0.0,
            chain_score: 0.0,
            context_score: 0.0,
            path_attack: 0.0,
            path_chain: 0.0,
            path_context: 0.0,
            path_clear_events: SmallVec::new(),
        }
    }

    #[test]
    fn test_reported_s_case_is_flagged_as_rotation_artifact() {
        let mut board = Board::new();
        board.rows[0] = 1015;
        board.rows[1] = 1011;
        board.rows[2] = 1019;
        for x in 0..COL_NB {
            board.cols[x] = board.col(x);
        }

        let mv = Move::new(Piece::S, Rotation::East, 2, 1, false);
        assert!(
            is_root_rotation_artifact(&board, mv),
            "reported no-shift S rotation-loop route should be filtered"
        );

        let mini = Move::new_allspin_mini(Piece::S, Rotation::East, 2, 1);
        assert!(
            is_root_rotation_artifact(&board, mini),
            "spin-labeled S root that normalizes to the same artifact should be filtered"
        );
    }

    #[test]
    fn test_expand_root_filters_reported_s_case_move() {
        let mut board = Board::new();
        board.rows[0] = 1015;
        board.rows[1] = 1011;
        board.rows[2] = 1019;
        for x in 0..COL_NB {
            board.cols[x] = board.col(x);
        }

        let state = GameState::new(
            board,
            Piece::S,
            vec![
                Piece::T,
                Piece::I,
                Piece::L,
                Piece::J,
                Piece::O,
                Piece::Z,
                Piece::S,
            ],
        );
        let config = SearchConfig {
            beam_width: 800,
            depth: 14,
            futility_delta: 15.0,
            time_budget_ms: Some(50),
            use_tt: false,
            extend_queue_7bag: true,
            attack_weight: 0.5,
            chain_weight: 1.0,
            context_weight: 0.1,
            board_weight: 1.0,
            quiescence_max_extensions: 3,
            quiescence_beam_fraction: 0.15,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let mut tt = Some(TranspositionTable::new(DEFAULT_TT_SIZE));
        let mut ctx = SearchExpansionContext {
            config: &config,
            weights: &weights,
            remaining_depth: 13,
            zobrist_keys: get_zobrist_keys(),
            tt: &mut tt,
        };

        let target = Move::new(Piece::S, Rotation::East, 2, 1, false);
        let roots = expand_root(&state, &mut ctx);
        assert!(
            !roots.iter().any(|n| n.root_move.raw() == target.raw()),
            "reported artifact root move should be removed at root expansion"
        );
    }

    #[test]
    fn test_reported_z_case_is_flagged_as_rotation_artifact() {
        let mut board = Board::new();
        board.rows[0] = 1007;
        board.rows[1] = 975;
        board.rows[2] = 991;
        board.rows[3] = 990;
        board.rows[4] = 959;
        board.rows[5] = 574;
        board.rows[6] = 794;
        board.rows[7] = 520;
        board.rows[8] = 8;
        for x in 0..COL_NB {
            board.cols[x] = board.col(x);
        }

        let mv = Move::new(Piece::Z, Rotation::East, 4, 1, false);
        assert!(
            is_root_rotation_artifact(&board, mv),
            "reported no-shift Z rotation-loop route should be filtered"
        );
    }

    #[test]
    fn test_expand_root_filters_reported_z_case_move() {
        let mut board = Board::new();
        board.rows[0] = 1007;
        board.rows[1] = 975;
        board.rows[2] = 991;
        board.rows[3] = 990;
        board.rows[4] = 959;
        board.rows[5] = 574;
        board.rows[6] = 794;
        board.rows[7] = 520;
        board.rows[8] = 8;
        for x in 0..COL_NB {
            board.cols[x] = board.col(x);
        }

        let mut state = GameState::new(
            board,
            Piece::Z,
            vec![
                Piece::J,
                Piece::T,
                Piece::O,
                Piece::Z,
                Piece::L,
                Piece::T,
                Piece::I,
                Piece::J,
                Piece::O,
                Piece::S,
                Piece::Z,
            ],
        );
        state.hold = Some(Piece::L);
        state.b2b = 12;

        let config = SearchConfig {
            beam_width: 800,
            depth: 14,
            futility_delta: 15.0,
            time_budget_ms: Some(50),
            use_tt: false,
            extend_queue_7bag: true,
            attack_weight: 0.5,
            chain_weight: 10.0,
            context_weight: 0.1,
            board_weight: 1.0,
            quiescence_max_extensions: 3,
            quiescence_beam_fraction: 0.15,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let mut tt = Some(TranspositionTable::new(DEFAULT_TT_SIZE));
        let mut ctx = SearchExpansionContext {
            config: &config,
            weights: &weights,
            remaining_depth: 13,
            zobrist_keys: get_zobrist_keys(),
            tt: &mut tt,
        };

        let target = Move::new(Piece::Z, Rotation::East, 4, 1, false);
        let roots = expand_root(&state, &mut ctx);
        assert!(
            !roots.iter().any(|n| n.root_move.raw() == target.raw()),
            "reported Z artifact root move should be removed at root expansion"
        );
    }

    #[test]
    fn test_reported_z_softdrop_rotation_loop_is_flagged() {
        let mut board = Board::new();
        board.rows[0] = 1007;
        board.rows[1] = 975;
        board.rows[2] = 991;
        board.rows[3] = 927;
        board.rows[4] = 831;
        board.rows[5] = 911;
        board.rows[6] = 415;
        board.rows[7] = 924;
        for x in 0..COL_NB {
            board.cols[x] = board.col(x);
        }

        let mv = Move::new(Piece::Z, Rotation::East, 4, 1, false);
        assert!(
            is_root_rotation_artifact(&board, mv),
            "reported Z softdrop+rotation loop should be filtered"
        );
    }

    #[test]
    fn test_expand_root_filters_reported_z_softdrop_rotation_loop() {
        let mut board = Board::new();
        board.rows[0] = 1007;
        board.rows[1] = 975;
        board.rows[2] = 991;
        board.rows[3] = 927;
        board.rows[4] = 831;
        board.rows[5] = 911;
        board.rows[6] = 415;
        board.rows[7] = 924;
        for x in 0..COL_NB {
            board.cols[x] = board.col(x);
        }

        let mut state = GameState::new(
            board,
            Piece::Z,
            vec![
                Piece::I,
                Piece::L,
                Piece::Z,
                Piece::O,
                Piece::J,
                Piece::T,
                Piece::S,
            ],
        );
        state.hold = Some(Piece::S);
        state.b2b = 2;

        let config = SearchConfig {
            beam_width: 800,
            depth: 14,
            futility_delta: 15.0,
            time_budget_ms: Some(50),
            use_tt: false,
            extend_queue_7bag: true,
            attack_weight: 0.5,
            chain_weight: 1.0,
            context_weight: 0.1,
            board_weight: 1.0,
            quiescence_max_extensions: 3,
            quiescence_beam_fraction: 0.15,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let mut tt = Some(TranspositionTable::new(DEFAULT_TT_SIZE));
        let mut ctx = SearchExpansionContext {
            config: &config,
            weights: &weights,
            remaining_depth: 13,
            zobrist_keys: get_zobrist_keys(),
            tt: &mut tt,
        };

        let target = Move::new(Piece::Z, Rotation::East, 4, 1, false);
        let roots = expand_root(&state, &mut ctx);
        assert!(
            !roots.iter().any(|n| n.root_move.raw() == target.raw()),
            "reported Z softdrop+rotation artifact should be removed at root expansion"
        );
    }

    #[test]
    fn test_find_best_move_empty_board() {
        let state = GameState::new(Board::new(), Piece::T, vec![Piece::I, Piece::O]);
        let config = SearchConfig::default();
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(result.is_some(), "should find a move on empty board");

        let r = result.unwrap_or_else(|| panic!("already checked"));
        assert!(!r.pv.is_empty(), "PV should have at least one move");
    }

    #[test]
    fn test_result_move_is_valid() {
        let state = GameState::new(Board::new(), Piece::I, vec![Piece::T]);
        let config = SearchConfig {
            beam_width: 100,
            depth: 1,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights)
            .unwrap_or_else(|| panic!("should find moves"));

        // verify the move can be applied
        let m = &result.best_move;
        let board = Board::new();
        assert!(
            !board.obstructed_move(m),
            "best move should be valid placement"
        );
    }

    #[test]
    fn test_depth_1_returns_immediately() {
        let state = GameState::new(
            Board::new(),
            Piece::S,
            vec![], // no queue
        );
        let config = SearchConfig {
            beam_width: 50,
            depth: 1,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(result.is_some());
        let r = result.unwrap_or_else(|| panic!("checked"));
        assert_eq!(r.pv.len(), 1, "depth-1 search should have single-move PV");
    }

    #[test]
    fn test_hold_swap_considered() {
        // set up a state where holding might help
        // T piece current, I piece in hold — I piece tetris should be considered
        let mut state = GameState::new(
            Board::new(),
            Piece::O, // O is least flexible
            vec![Piece::S],
        );
        state.hold = Some(Piece::I); // I is great for tetrises

        let config = SearchConfig {
            beam_width: 200,
            depth: 1,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(result.is_some(), "should find a move with hold available");
    }

    #[test]
    fn test_hold_none_does_not_create_hold_move() {
        let state = GameState::new(Board::new(), Piece::T, vec![Piece::I, Piece::O]);
        let config = SearchConfig {
            beam_width: 200,
            depth: 2,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(result.is_some());
        let r = result.unwrap_or_else(|| panic!("checked"));
        assert!(
            !r.hold_used,
            "hold cannot be used when hold slot is empty"
        );
        assert!(r.pv.len() <= 2, "PV shouldn't exceed depth");
    }

    #[test]
    fn test_beam_width_respected() {
        let state = GameState::new(Board::new(), Piece::T, vec![Piece::I, Piece::O, Piece::L]);
        // very narrow beam
        let config = SearchConfig {
            beam_width: 3,
            depth: 3,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(result.is_some(), "narrow beam should still find something");
    }

    #[test]
    fn test_bag_extends_search_depth() {
        let mut state = GameState::new(Board::new(), Piece::O, vec![Piece::T, Piece::L, Piece::J]);
        state.hold = Some(Piece::I);

        let weights = EvalWeights::default();
        let baseline_config = SearchConfig {
            beam_width: 200,
            depth: 6,
            extend_queue_7bag: false,
            ..SearchConfig::default()
        };
        let extended_config = SearchConfig {
            beam_width: 200,
            depth: 6,
            extend_queue_7bag: true,
            ..SearchConfig::default()
        };

        let baseline = find_best_move(&state, &baseline_config, &weights)
            .unwrap_or_else(|| panic!("baseline search should return a move"));
        let extended = find_best_move(&state, &extended_config, &weights)
            .unwrap_or_else(|| panic!("extended search should return a move"));
        let extended_queue = bag::extend_queue(&state.queue, state.current, state.hold);

        assert!(extended_queue.len() > state.queue.len());
        assert_eq!(
            baseline.pv.len(),
            baseline_config.depth.min(state.queue.len() + 1),
            "baseline depth should use visible queue only"
        );
        assert_eq!(
            extended.pv.len(),
            extended_config.depth.min(extended_queue.len() + 1),
            "extended depth should use 7-bag queue extension"
        );
    }

    #[test]
    fn test_tt_deduplicates() {
        let mut state = GameState::new(
            Board::new(),
            Piece::O,
            vec![Piece::T, Piece::L, Piece::J, Piece::S],
        );
        state.hold = Some(Piece::I);

        let weights = EvalWeights::default();
        let baseline_config = SearchConfig {
            beam_width: 250,
            depth: 5,
            use_tt: false,
            extend_queue_7bag: false,
            ..SearchConfig::default()
        };
        let tt_config = SearchConfig {
            beam_width: 250,
            depth: 5,
            use_tt: true,
            extend_queue_7bag: false,
            ..SearchConfig::default()
        };

        let baseline = find_best_move(&state, &baseline_config, &weights)
            .unwrap_or_else(|| panic!("baseline search should return a move"));
        let with_tt = find_best_move(&state, &tt_config, &weights)
            .unwrap_or_else(|| panic!("tt search should return a move"));

        assert_eq!(with_tt.best_move, baseline.best_move);
        assert_eq!(with_tt.hold_used, baseline.hold_used);
    }

    #[test]
    fn test_no_moves_returns_none() {
        // fill the board nearly to the top — no valid placements
        let mut board = Board::new();
        for y in 0..40 {
            board.rows[y] = FULL_ROW;
        }
        for x in 0..COL_NB {
            board.cols[x] = !0u64; // all bits set
        }

        let state = GameState::new(board, Piece::I, vec![]);
        let config = SearchConfig::default();
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(result.is_none(), "full board should have no moves");
    }

    #[test]
    fn test_futility_prunes_bad_moves() {
        let mut nodes = vec![
            SearchNode {
                board: Board::new(),
                score: 10.0,
                hold: None,
                b2b: 0,
                combo: 0,
                pending_garbage: 0,
                coaching: CoachingState::default(),
                root_move: Move::none(),
                root_hold_used: false,
                path: smallvec![Move::none()],
                board_score: 0.0,
                attack_score: 0.0,
                chain_score: 0.0,
                context_score: 0.0,
                path_attack: 0.0,
                path_chain: 0.0,
                path_context: 0.0,
                path_clear_events: SmallVec::new(),
            },
            SearchNode {
                board: Board::new(),
                score: 8.5,
                hold: None,
                b2b: 0,
                combo: 0,
                pending_garbage: 0,
                coaching: CoachingState::default(),
                root_move: Move::none(),
                root_hold_used: false,
                path: smallvec![Move::none()],
                board_score: 0.0,
                attack_score: 0.0,
                chain_score: 0.0,
                context_score: 0.0,
                path_attack: 0.0,
                path_chain: 0.0,
                path_context: 0.0,
                path_clear_events: SmallVec::new(),
            },
            SearchNode {
                board: Board::new(),
                score: 5.0,
                hold: None,
                b2b: 0,
                combo: 0,
                pending_garbage: 0,
                coaching: CoachingState::default(),
                root_move: Move::none(),
                root_hold_used: false,
                path: smallvec![Move::none()],
                board_score: 0.0,
                attack_score: 0.0,
                chain_score: 0.0,
                context_score: 0.0,
                path_attack: 0.0,
                path_chain: 0.0,
                path_context: 0.0,
                path_clear_events: SmallVec::new(),
            },
        ];

        apply_futility_pruning(&mut nodes, 3.0, None);

        assert_eq!(nodes.len(), 2, "score 5.0 should be pruned");
        assert!(nodes.iter().all(|node| node.score >= 7.0));
    }

    #[test]
    fn test_iterative_widening_returns_result() {
        let state = GameState::new(
            Board::new(),
            Piece::T,
            vec![Piece::I, Piece::O, Piece::L, Piece::J],
        );
        let config = SearchConfig {
            beam_width: 400,
            depth: 4,
            time_budget_ms: Some(100),
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights);
        assert!(
            result.is_some(),
            "iterative widening should return a result"
        );

        let r = result.unwrap_or_else(|| panic!("checked"));
        assert!(!r.pv.is_empty(), "PV should include at least one move");
    }

    #[test]
    fn test_compare_prefers_survival_before_raw_score() {
        let nodes = &mut [
            make_node(
                999.0,
                crate::state::FatalityState::Critical,
                crate::state::ObligationState::MustCancel,
            ),
            make_node(
                10.0,
                crate::state::FatalityState::Safe,
                crate::state::ObligationState::None,
            ),
        ];

        nodes.sort_unstable_by(compare_nodes_desc);

        assert_eq!(
            nodes[0].coaching.fatality,
            crate::state::FatalityState::Safe
        );
        assert_eq!(
            nodes[0].coaching.obligation,
            crate::state::ObligationState::None
        );
        assert_eq!(
            nodes[1].coaching.fatality,
            crate::state::FatalityState::Critical
        );
    }

    #[test]
    fn test_futility_preserves_best_survival_tier() {
        let mut nodes = vec![
            make_node(
                1000.0,
                crate::state::FatalityState::Critical,
                crate::state::ObligationState::MustCancel,
            ),
            make_node(
                12.0,
                crate::state::FatalityState::Safe,
                crate::state::ObligationState::MustDownstack,
            ),
            make_node(
                9.0,
                crate::state::FatalityState::Safe,
                crate::state::ObligationState::None,
            ),
            make_node(
                4.0,
                crate::state::FatalityState::Safe,
                crate::state::ObligationState::None,
            ),
        ];

        apply_futility_pruning(&mut nodes, 3.0, None);

        assert!(nodes.iter().all(|n| {
            n.coaching.fatality == crate::state::FatalityState::Safe
                && n.coaching.obligation == crate::state::ObligationState::None
        }));
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].score, 9.0);
    }

    #[test]
    fn test_must_cancel_detected_from_imminent_garbage() {
        let mut state = GameState::new(Board::new(), Piece::T, vec![Piece::I, Piece::O]);
        state.pending_garbage = 4;

        let config = SearchConfig {
            beam_width: 100,
            depth: 1,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights)
            .unwrap_or_else(|| panic!("expected a legal move"));

        assert_eq!(
            result.coaching_state.obligation,
            crate::state::ObligationState::MustCancel
        );
    }

    #[test]
    fn test_spawn_envelope_violation_forces_fatal_tier() {
        let mut board = Board::new();
        board.rows[crate::default_ruleset::ACTIVE_RULES.spawn_row as usize] = 1u16 << 4;
        board.cols = board.compute_cols();

        let state = GameState::new(board, Piece::T, vec![Piece::I, Piece::O]);
        let config = SearchConfig {
            beam_width: 100,
            depth: 1,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let result = find_best_move(&state, &config, &weights)
            .unwrap_or_else(|| panic!("expected a legal move"));

        assert_eq!(
            result.coaching_state.fatality,
            crate::state::FatalityState::Fatal
        );
    }

    #[test]
    fn test_position_complexity_varies() {
        let state = GameState::new(Board::new(), Piece::T, vec![Piece::I, Piece::O]);
        let config = SearchConfig {
            beam_width: 200,
            depth: 2,
            ..SearchConfig::default()
        };
        let weights = EvalWeights::default();

        let full = find_best_move_with_scores(&state, &config, &weights)
            .unwrap_or_else(|| panic!("should find moves"));

        // On a clean board with multiple root moves, complexity should be >= 0
        assert!(
            full.position_complexity >= 0.0,
            "position_complexity should be non-negative, got {}",
            full.position_complexity
        );
    }
}
