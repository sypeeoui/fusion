// pathfinder.rs -- 1:1 port of pathfinder.hpp + pathfinder.cpp
#![allow(dead_code)]
#![allow(clippy::enum_variant_names)] // NoInput variant triggers this

use std::collections::VecDeque;

use crate::board::Board;
use crate::default_ruleset::ACTIVE_RULES;
use crate::gen::*;
use crate::header::*;

// -- Input --

pub const MAX_INPUTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Input {
    NoInput = 0,
    ShiftRight,
    ShiftLeft,
    DasRight,
    DasLeft,
    RotateCw,
    RotateCcw,
    RotateFlip,
    SoftDrop,
    HardDrop,
}

// -- Inputs --

#[derive(Clone, Debug)]
pub struct Inputs {
    pub(crate) data: Vec<Input>,
}

impl Inputs {
    pub fn new() -> Self {
        Inputs { data: Vec::new() }
    }

    pub(crate) fn push(&mut self, input: Input) {
        self.data.push(input);
    }

    pub(crate) fn reverse(&mut self) {
        self.data.reverse();
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn as_u8_vec(&self) -> Vec<u8> {
        self.data.iter().map(|i| *i as u8).collect()
    }
}

impl Default for Inputs {
    fn default() -> Self {
        Self::new()
    }
}

// -- PathNode --

struct PathNode {
    input: Input,
    prev: u16,
}

// -- GhostMove --

struct GhostMove {
    r: Rotation,
    x: i8,
    y: i8,
    i: u16,
    s: SpinType,
}

impl GhostMove {
    fn root_index() -> u16 {
        u16::MAX
    }
}

// -- get_input --

pub fn get_input(board: &Board, target: &Move, use_finesse: bool, force: bool) -> Inputs {
    get_input_inner(board, target, use_finesse, force, target.piece())
}

fn get_input_inner(
    board: &Board,
    target: &Move,
    _use_finesse: bool,
    force: bool,
    p: Piece,
) -> Inputs {
    let cols = board.compute_cols();
    let cm = CollisionMap::new(&cols, p);
    let is_t = p == Piece::T && ACTIVE_RULES.enable_tspin;
    let is_allspin = p != Piece::T && p != Piece::O && ACTIVE_RULES.enable_allspin;
    let can_spin = is_t || is_allspin;
    let spin_nb = if can_spin { SPIN_NB } else { 1 };

    // searched[spin][col][rot] bitboard
    let mut searched = vec![vec![vec![0u64; ROTATION_NB]; COL_NB]; spin_nb];

    let mut vec: Vec<PathNode> = Vec::new();
    let mut queue: VecDeque<GhostMove> = VecDeque::new();

    // spawn
    let spawn_y = if force {
        // find lowest valid row >= spawn_row
        let blocked = cm.get(SPAWN_COL, Rotation::North);
        let above_spawn = !bb_low(ACTIVE_RULES.spawn_row);
        let valid = !blocked & above_spawn;
        if valid == 0 {
            return Inputs::new();
        }
        ctz(valid) as i8
    } else {
        if cm.get(SPAWN_COL, Rotation::North) & bb(ACTIVE_RULES.spawn_row) != 0 {
            return Inputs::new();
        }
        ACTIVE_RULES.spawn_row as i8
    };

    searched[0][SPAWN_COL][Rotation::North as usize] |= bb(spawn_y as i32);
    queue.push_back(GhostMove {
        r: Rotation::North,
        x: SPAWN_COL as i8,
        y: spawn_y,
        i: GhostMove::root_index(),
        s: SpinType::NoSpin,
    });

    while let Some(m) = queue.pop_front() {
        let x = m.x as usize;
        let r = m.r;
        let y = m.y;
        let rc = canonical_r(p, r);

        // harddrop
        let drop_mask = !((!cm.get(x, rc)) << (63 - y as u32));
        let drop_y = (clz(drop_mask) as i8) - 1;

        if drop_y >= 0 {
            let mut s = m.s;
            if can_spin {
                s = SpinType::NoSpin;
            }
            let sc = if can_spin { s as usize } else { 0 };

            // check if this harddrop position == target
            let target_r = target.rotation();
            let target_rc = canonical_r(p, target_r);
            if x as i32 == target.x() && drop_y as i32 == target.y() && rc == target_rc {
                // check spin match
                let target_spin = target.spin();
                if !can_spin || sc == target_spin as usize {
                    // trace back path
                    let mut result = Inputs::new();
                    result.push(Input::HardDrop);
                    let mut idx = m.i;
                    while idx != GhostMove::root_index() {
                        result.push(vec[idx as usize].input);
                        idx = vec[idx as usize].prev;
                    }
                    result.reverse();
                    return result;
                }
            }
        }

        // rotate
        if p != Piece::O {
            let dirs = if ACTIVE_RULES.enable_180 { 3 } else { 2 };
            for d_idx in 0..dirs {
                let d = match d_idx {
                    0 => Direction::Cw,
                    1 => Direction::Ccw,
                    _ => Direction::Flip,
                };
                let input = match d {
                    Direction::Cw => Input::RotateCw,
                    Direction::Ccw => Input::RotateCcw,
                    Direction::Flip => Input::RotateFlip,
                };

                let rt = rotate(d, r);
                let off = canonical_offset(p, r) - canonical_offset(p, rt);

                let mut kick_buf = [Coordinates::new(0, 0); 6];
                let kick_count = if d == Direction::Flip {
                    let ki = kick_180_index(p);
                    let arr = &KICKS_180[ki][r as usize];
                    let n = if p == Piece::I { 2 } else { arr.len() };
                    kick_buf[..n].copy_from_slice(&arr[..n]);
                    n
                } else {
                    let ki = kick_index(p, ACTIVE_RULES.srs_plus);
                    let arr = &KICKS[ki][d as usize][r as usize];
                    kick_buf[..arr.len()].copy_from_slice(arr);
                    arr.len()
                };

                for (_k, &kick) in kick_buf.iter().enumerate().take(kick_count) {
                    let x1 = m.x as i32 + kick.x as i32 + off.x as i32;
                    let y1 = y as i32 + kick.y as i32 + off.y as i32;

                    if x1 < 0 || y1 < 0 {
                        continue;
                    }
                    let x1u = x1 as usize;
                    if !in_bounds(p, rt, x1 as i32) {
                        continue;
                    }
                    if y1 >= ROW_NB as i32 {
                        continue;
                    }

                    let rt_c = canonical_r(p, rt);
                    if cm.get(x1u, rt_c) & bb(y1) != 0 {
                        continue;
                    }

                    // Spin detection
                    let mut s = SpinType::NoSpin;
                    if is_t {
                        // T-piece: 3-corner check
                        let ty = y1;
                        let tx = x1;
                        let mut corners = 0u32;
                        for &(dx, dy) in &[(-1i32, -1i32), (1, -1), (-1, 1), (1, 1)] {
                            let cx = tx + dx;
                            let cy = ty + dy;
                            if cx < 0 || cx >= COL_NB as i32 || cy < 0 || board.occupied(cx, cy) {
                                corners += 1;
                            }
                        }
                        if corners >= 3 {
                            // face corner check for FULL vs MINI
                            let face = match rt {
                                Rotation::North => [(-1i32, 1i32), (1, 1)],
                                Rotation::East => [(1, 1), (1, -1)],
                                Rotation::South => [(1, -1), (-1, -1)],
                                Rotation::West => [(-1, -1), (-1, 1)],
                            };
                            let mut face_filled = 0u32;
                            for &(dx, dy) in &face {
                                let fx = tx + dx;
                                let fy = ty + dy;
                                if fx < 0 || fx >= COL_NB as i32 || fy < 0 || board.occupied(fx, fy)
                                {
                                    face_filled += 1;
                                }
                            }
                            s = if face_filled >= 2 {
                                SpinType::Full
                            } else {
                                SpinType::Mini
                            };
                        }
                    } else if is_allspin {
                        // Non-T allspin: 4-direction immobility check
                        let rt_c = canonical_r(p, rt);
                        let blocked_left = x1u == 0 || cm.get(x1u - 1, rt_c) & bb(y1) != 0;
                        let blocked_right = x1u >= COL_NB - 1 || cm.get(x1u + 1, rt_c) & bb(y1) != 0;
                        let blocked_down = y1 <= 0 || cm.get(x1u, rt_c) & bb(y1 - 1) != 0;
                        let blocked_up = y1 >= ROW_NB as i32 - 1 || cm.get(x1u, rt_c) & bb(y1 + 1) != 0;
                        if blocked_left && blocked_right && blocked_down && blocked_up {
                            s = SpinType::Mini;
                        }
                    }

                    let s_idx = if can_spin { s as usize } else { 0 };
                    let rt_c_idx = canonical_r(p, rt) as usize;

                    if searched[s_idx][x1u][rt_c_idx] & bb(y1) != 0 {
                        continue;
                    }
                    searched[s_idx][x1u][rt_c_idx] |= bb(y1);

                    let node_idx = vec.len() as u16;
                    vec.push(PathNode { input, prev: m.i });
                    queue.push_back(GhostMove {
                        r: rt,
                        x: x1 as i8,
                        y: y1 as i8,
                        i: node_idx,
                        s,
                    });
                    break; // first valid kick wins
                }
            }
        }

        // shift
        for dx in [-1i8, 1i8] {
            let x1 = m.x as i32 + dx as i32;
            if x1 >= 0 && in_bounds(p, r, x1) {
                let x1u = x1 as usize;
                let rc = canonical_r(p, r);
                if cm.get(x1u, rc) & bb(y as i32) == 0 {
                    let s_idx = if can_spin {
                        SpinType::NoSpin as usize
                    } else {
                        0
                    };
                    let rc_idx = rc as usize;

                    if searched[s_idx][x1u][rc_idx] & bb(y as i32) == 0 {
                        searched[s_idx][x1u][rc_idx] |= bb(y as i32);

                        let input = if dx < 0 {
                            Input::ShiftLeft
                        } else {
                            Input::ShiftRight
                        };
                        let node_idx = vec.len() as u16;
                        vec.push(PathNode { input, prev: m.i });
                        queue.push_back(GhostMove {
                            r,
                            x: x1 as i8,
                            y,
                            i: node_idx,
                            s: SpinType::NoSpin,
                        });
                    }
                }
            }

            // DAS
            let mut x_das = m.x;
            let mut das_moved = false;
            let rc = canonical_r(p, r);
            while in_bounds(p, r, x_das as i32 + dx as i32) {
                let next_x = (x_das as i32 + dx as i32) as usize;
                if cm.get(next_x, rc) & bb(y as i32) != 0 {
                    break;
                }
                x_das = next_x as i8;
                das_moved = true;
            }
            if das_moved {
                let x_das_u = x_das as usize;
                let s_idx = if can_spin {
                    SpinType::NoSpin as usize
                } else {
                    0
                };
                let rc_idx = rc as usize;
                if searched[s_idx][x_das_u][rc_idx] & bb(y as i32) == 0 {
                    searched[s_idx][x_das_u][rc_idx] |= bb(y as i32);
                    let input = if dx < 0 {
                        Input::DasLeft
                    } else {
                        Input::DasRight
                    };
                    let node_idx = vec.len() as u16;
                    vec.push(PathNode { input, prev: m.i });
                    queue.push_back(GhostMove {
                        r,
                        x: x_das,
                        y,
                        i: node_idx,
                        s: SpinType::NoSpin,
                    });
                }
            }
        }

        // softdrop
        let mut y_sd = y;
        let rc = canonical_r(p, r);
        while y_sd > 0 {
            let next_y = y_sd - 1;
            if cm.get(x, rc) & bb(next_y as i32) != 0 {
                break;
            }
            
            let s_idx = if can_spin {
                SpinType::NoSpin as usize
            } else {
                0
            };
            let rc_idx = rc as usize;
            
            // For SoftDrop, we only want to add a node for the TARGET position in the BFS,
            // but we want the path to include all intermediate SoftDrop steps.
            // Wait, BFS finds the SHORTEST path. If we want to reach a low Y,
            // we should allow the BFS to see each intermediate Y as a reachable state.
            
            y_sd = next_y;
            
            if searched[s_idx][x][rc_idx] & bb(y_sd as i32) == 0 {
                searched[s_idx][x][rc_idx] |= bb(y_sd as i32);
                let node_idx = vec.len() as u16;
                vec.push(PathNode {
                    input: Input::SoftDrop,
                    prev: m.i,
                });
                queue.push_back(GhostMove {
                    r,
                    x: m.x,
                    y: y_sd,
                    i: node_idx,
                    s: SpinType::NoSpin,
                });
                
                // We DON'T break here, because we want to explore deeper softdrops 
                // but each one will be its own node in the BFS.
                // Wait, if we don't break, the BFS will explore them anyway 
                // when it pops the next GhostMove. 
                // So we SHOULD break here after adding the NEXT row.
                break; 
            }
        }
    }

    // target not found
    Inputs::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_input_simple_i_drop() {
        let board = Board::new();
        let target = Move::new(Piece::I, Rotation::North, SPAWN_COL as i32, 0, false);
        let inputs = get_input(&board, &target, false, false);
        assert!(!inputs.data.is_empty());
        assert_eq!(*inputs.data.last().unwrap(), Input::HardDrop);
    }

    #[test]
    fn test_get_input_t_piece() {
        let board = Board::new();
        let target = Move::new(Piece::T, Rotation::North, SPAWN_COL as i32, 0, false);
        let inputs = get_input(&board, &target, false, false);
        assert!(!inputs.data.is_empty());
        assert_eq!(*inputs.data.last().unwrap(), Input::HardDrop);
    }
}
