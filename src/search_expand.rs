use crate::analysis::{assemble_composite, shape_chain_value, shape_context_modifier};
use crate::attack::{calculate_attack_full, AttackContext};
use crate::board::Board;
use crate::eval::{evaluate, EvalWeights};
use crate::header::{Move, Piece};
use crate::move_buffer::MoveBuffer;
use crate::movegen::generate;
use crate::search_config::{SearchExpansionContext, SearchNode};
use crate::state::{
    ClearEvent, ClearType, CoachingState, FatalityState, GameState, ObligationState, PhaseState,
    SurgeState, TransitionObservation,
};
use crate::transposition::{TranspositionTable, ZobristKeys};
use smallvec::{smallvec, SmallVec};

#[inline]
fn coaching_context_bias(previous: CoachingState, next: CoachingState) -> f32 {
    fn score(state: CoachingState) -> f32 {
        let fatality = match state.fatality {
            FatalityState::Safe => 0.0,
            FatalityState::Critical => -0.35,
            FatalityState::Fatal => -0.70,
        };
        let obligation = match state.obligation {
            ObligationState::None => 0.0,
            ObligationState::MustDownstack => -0.25,
            ObligationState::MustCancel => -0.45,
        };
        let surge = match state.surge {
            SurgeState::Dormant => 0.0,
            SurgeState::Building => 0.20,
            SurgeState::Active => 0.35,
        };
        let phase = match state.phase {
            PhaseState::Opener => 0.10,
            PhaseState::Midgame => 0.0,
            PhaseState::Endgame => -0.10,
        };
        fatality + obligation + surge + phase
    }

    score(next) - score(previous)
}

pub(crate) fn gen_and_eval_root(
    state: &GameState,
    piece: Piece,
    new_hold: Option<Piece>,
    hold_used: bool,
    ctx: &mut SearchExpansionContext<'_>,
    nodes: &mut Vec<SearchNode>,
) {
    let mut moves = MoveBuffer::new();
    generate(&state.board, &mut moves, piece, true);

    for m in moves.as_slice() {
        if !state.board.legal_lock_placement(m) {
            continue;
        }

        let mut result_board = state.board.clone();
        let lines_cleared = result_board.do_move(m) as u8;
        let next_pending_garbage = state.pending_garbage.saturating_sub(lines_cleared);
        let spawn_envelope_blocked = GameState::spawn_envelope_blocked(&result_board);
        let is_perfect_clear = result_board.is_empty();

        let (next_b2b, next_combo) =
            GameState::next_chain_values(state.b2b, state.combo, m, lines_cleared, is_perfect_clear);
        let coaching = state.coaching.transition(TransitionObservation {
            resulting_height: result_board.height(),
            resulting_b2b: next_b2b,
            resulting_combo: next_combo,
            lines_cleared,
            hold_used,
            pending_garbage: state.pending_garbage,
            imminent_garbage: next_pending_garbage,
            spawn_envelope_blocked,
        });

        let board_eval = evaluate_with_tt(
            &result_board,
            ctx.weights,
            ctx.remaining_depth,
            ctx.zobrist_keys,
            ctx.tt,
        );
        // Detect B2B chain break for surge release
        let b2b_broken_from = if state.b2b >= 4 && next_b2b == 0 && lines_cleared > 0 {
            Some(state.b2b)
        } else {
            None
        };
        let clears_garbage = state.pending_garbage > 0 && lines_cleared > 0;
        let attack_val = calculate_attack_full(&AttackContext {
            lines: lines_cleared,
            spin: m.spin(),
            b2b: next_b2b,
            combo: next_combo as u8,
            config: &ctx.config.attack_config,
            is_perfect_clear,
            b2b_broken_from,
            clears_garbage,
        });

        // B2B breaking penalty: if b2b was active and is now broken by a non-difficult clear
        let b2b_break_penalty = if state.b2b > 0 && next_b2b == 0 && lines_cleared > 0 {
            state.b2b as f32 * ctx.config.b2b_weight
        } else {
            0.0
        };

        let clear_event = if lines_cleared > 0 {
            Some(ClearEvent {
                clear_type: ClearType::from_lines(lines_cleared),
                spin_type: m.spin(),
                lines_cleared,
                attack_sent: attack_val,
                b2b_before: state.b2b,
                b2b_after: next_b2b,
                combo_before: state.combo,
                combo_after: next_combo,
                is_surge_release: b2b_broken_from.is_some(),
                is_garbage_clear: clears_garbage,
                is_perfect_clear,
                piece,
            })
        } else {
            None
        };
        let path_clear_events = match clear_event {
            Some(event) => smallvec![event],
            None => SmallVec::new(),
        };
        let chain_val = shape_chain_value(next_combo as f32);
        let combo_context = next_combo as f32 - state.combo as f32;
        let context_mod = shape_context_modifier(
            combo_context + coaching_context_bias(state.coaching, coaching),
        );

        let height_decrease = (state.board.height() as f32 - result_board.height() as f32).max(0.0);
        
        // Lowest part of the stack = min(highest occupied row across all columns)
        let mut col_h = [0usize; 10];
        let mut total_blocks = 0u32;
        for x in 0..10 {
            let col_bits = result_board.cols[x];
            col_h[x] = if col_bits == 0 { 0 } else { (64 - col_bits.leading_zeros()) as usize };
            total_blocks += col_bits.count_ones();
        }
        let min_h = col_h.iter().copied().min().unwrap_or(0);
        let avg_h = col_h.iter().sum::<usize>() as f32 / 10.0;
        let max_h = result_board.height();

        // Prioritize paths that reach lower absolute max height, lower absolute min height, and have fewer blocks
        let downstack_val = height_decrease * 10.0 
            + (40.0 - max_h as f32) * 2.0 
            + avg_h * ctx.config.downstack_avg_height_weight 
            + min_h as f32 * ctx.config.downstack_min_height_weight 
            + (400.0 - total_blocks as f32) * 0.1;

        let spin_structural_bonus = if m.is_full() {
            ctx.weights.spin_full
        } else if m.is_mini() {
            ctx.weights.spin_mini
        } else {
            0.0
        };

        let composite_score = assemble_composite(
            board_eval + spin_structural_bonus,
            attack_val,
            chain_val,
            next_b2b as f32,
            downstack_val,
            context_mod,
            ctx.config,
        ) - b2b_break_penalty;

        nodes.push(SearchNode {
            board: result_board,
            score: composite_score,
            hold: new_hold,
            b2b: next_b2b,
            combo: next_combo,
            pending_garbage: next_pending_garbage,
            coaching,
            root_move: *m,
            root_hold_used: hold_used,
            path: smallvec![*m],
            board_score: board_eval + spin_structural_bonus,
            attack_score: attack_val,
            chain_score: chain_val,
            b2b_score: next_b2b as f32,
            downstack_score: downstack_val,
            context_score: context_mod,
            path_attack: attack_val,
            path_chain: chain_val,
            path_downstack: downstack_val,
            path_context: context_mod,
            path_clear_events,
        });
    }
}

pub(crate) fn expand_node(
    parent: &SearchNode,
    piece: Piece,
    new_hold: Option<Piece>,
    hold_used: bool,
    ctx: &mut SearchExpansionContext<'_>,
    out: &mut Vec<SearchNode>,
) {
    let mut moves = MoveBuffer::new();
    generate(&parent.board, &mut moves, piece, true);

    for m in moves.as_slice() {
        if !parent.board.legal_lock_placement(m) {
            continue;
        }

        let mut result_board = parent.board.clone();
        let lines_cleared = result_board.do_move(m) as u8;
        let next_pending_garbage = parent.pending_garbage.saturating_sub(lines_cleared);
        let spawn_envelope_blocked = GameState::spawn_envelope_blocked(&result_board);
        let is_perfect_clear = result_board.is_empty();

        let (next_b2b, next_combo) =
            GameState::next_chain_values(parent.b2b, parent.combo, m, lines_cleared, is_perfect_clear);
        let coaching = parent.coaching.transition(TransitionObservation {
            resulting_height: result_board.height(),
            resulting_b2b: next_b2b,
            resulting_combo: next_combo,
            lines_cleared,
            hold_used,
            pending_garbage: parent.pending_garbage,
            imminent_garbage: next_pending_garbage,
            spawn_envelope_blocked,
        });

        let board_eval = evaluate_with_tt(
            &result_board,
            ctx.weights,
            ctx.remaining_depth,
            ctx.zobrist_keys,
            ctx.tt,
        );
        // Detect B2B chain break for surge release
        let b2b_broken_from = if parent.b2b >= 4 && next_b2b == 0 && lines_cleared > 0 {
            Some(parent.b2b)
        } else {
            None
        };
        let clears_garbage = parent.pending_garbage > 0 && lines_cleared > 0;
        let attack_val = calculate_attack_full(&AttackContext {
            lines: lines_cleared,
            spin: m.spin(),
            b2b: next_b2b,
            combo: next_combo as u8,
            config: &ctx.config.attack_config,
            is_perfect_clear,
            b2b_broken_from,
            clears_garbage,
        });

        // B2B breaking penalty: if b2b was active and is now broken by a non-difficult clear
        let b2b_break_penalty = if parent.b2b > 0 && next_b2b == 0 && lines_cleared > 0 {
            parent.b2b as f32 * ctx.config.b2b_weight
        } else {
            0.0
        };

        let clear_event = if lines_cleared > 0 {
            Some(ClearEvent {
                clear_type: ClearType::from_lines(lines_cleared),
                spin_type: m.spin(),
                lines_cleared,
                attack_sent: attack_val,
                b2b_before: parent.b2b,
                b2b_after: next_b2b,
                combo_before: parent.combo,
                combo_after: next_combo,
                is_surge_release: b2b_broken_from.is_some(),
                is_garbage_clear: clears_garbage,
                is_perfect_clear,
                piece,
            })
        } else {
            None
        };
        let mut path_clear_events = parent.path_clear_events.clone();
        if let Some(event) = clear_event {
            path_clear_events.push(event);
        }
        let chain_val = shape_chain_value(next_combo as f32);
        let combo_context = next_combo as f32 - parent.combo as f32;
        let context_mod = shape_context_modifier(
            combo_context + coaching_context_bias(parent.coaching, coaching),
        );

        let height_decrease = (parent.board.height() as f32 - result_board.height() as f32).max(0.0);
        let mut col_h = [0usize; 10];
        let mut total_blocks = 0u32;
        for x in 0..10 {
            let col_bits = result_board.cols[x];
            col_h[x] = if col_bits == 0 { 0 } else { (64 - col_bits.leading_zeros()) as usize };
            total_blocks += col_bits.count_ones();
        }
        let min_h = col_h.iter().copied().min().unwrap_or(0);
        let avg_h = col_h.iter().sum::<usize>() as f32 / 10.0;
        let max_h = result_board.height();
        let downstack_val = height_decrease * 10.0 
            + (40.0 - max_h as f32) * 2.0 
            + avg_h * ctx.config.downstack_avg_height_weight 
            + min_h as f32 * ctx.config.downstack_min_height_weight 
            + (400.0 - total_blocks as f32) * 0.1;

        let spin_structural_bonus = if m.is_full() {
            ctx.weights.spin_full
        } else if m.is_mini() {
            ctx.weights.spin_mini
        } else {
            0.0
        };

        let decay = ctx.config.path_decay;
        let cum_attack = parent.path_attack * decay + attack_val;
        let cum_chain = parent.path_chain * decay + chain_val;
        let cum_downstack = parent.path_downstack * decay + downstack_val;

        let depth_factor = (parent.path.len() as f32 + 1.0)
            .sqrt()
            .min(ctx.config.max_depth_factor);
        let composite_score = assemble_composite(
            board_eval + spin_structural_bonus,
            cum_attack / depth_factor,
            cum_chain / depth_factor,
            next_b2b as f32,
            cum_downstack / depth_factor,
            context_mod,
            ctx.config,
        ) - b2b_break_penalty;

        let mut path: SmallVec<[Move; 16]> = parent.path.clone();
        path.push(*m);

        out.push(SearchNode {
            board: result_board,
            score: composite_score,
            hold: new_hold,
            b2b: next_b2b,
            combo: next_combo,
            pending_garbage: next_pending_garbage,
            coaching,
            root_move: parent.root_move,
            root_hold_used: parent.root_hold_used,
            path,
            board_score: board_eval + spin_structural_bonus,
            attack_score: attack_val,
            chain_score: chain_val,
            b2b_score: next_b2b as f32,
            downstack_score: downstack_val,
            context_score: context_mod,
            path_attack: cum_attack,
            path_chain: cum_chain,
            path_downstack: cum_downstack,
            path_context: parent.path_context * decay + context_mod,
            path_clear_events,
        });
    }
}

pub(crate) fn evaluate_with_tt(
    board: &Board,
    weights: &EvalWeights,
    remaining_depth: usize,
    zobrist_keys: &ZobristKeys,
    tt: &mut Option<TranspositionTable>,
) -> f32 {
    if let Some(table) = tt.as_mut() {
        let depth = remaining_depth.min(u8::MAX as usize) as u8;
        let hash = zobrist_keys.hash_board(board);

        if let Some(score) = table.probe(hash, depth) {
            return score;
        }

        let score = evaluate(board, weights);
        table.store(hash, depth, score);
        return score;
    }

    evaluate(board, weights)
}
