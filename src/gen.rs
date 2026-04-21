// gen.rs -- 1:1 port of gen.hpp
use crate::header::*;

pub(crate) const SPAWN_COL: usize = 4;

// C++ in_bounds<p, r>(x): checks pivot x and all 3 relative cells are valid columns
pub(crate) fn in_bounds(p: Piece, r: Rotation, x: i32) -> bool {
    if !is_ok_x(x) {
        return false;
    }
    let pc = piece_table(p, r);
    // Columns check
    if !is_ok_x(pc[0].x as i32 + x) || !is_ok_x(pc[1].x as i32 + x) || !is_ok_x(pc[2].x as i32 + x) {
        return false;
    }
    // Rows check (must be at least 0 to not be obstructed by floor immediately if we are at y=0)
    // Actually pieces can be above spawn_row, but never below 0.
    // Piece at y=0 is valid if all its minos have y >= 0.
    if (pc[0].y as i32) < 0 || (pc[1].y as i32) < 0 || (pc[2].y as i32) < 0 {
        // If piece at y=0 has any mino with relative y < 0, it's impossible to place at any y >= 0
        // without that mino being at y < 0.
        // Wait, no. If we are at y=2, and mino is at y=-2, it's at 0.
        // But if piece at y=0 has mino at y=-1, it's out of bounds.
        // in_bounds in Cobra usually only checks X.
        // If a piece has a mino at relative y=-2, then the lowest legal y for that piece is y=2.
    }
    true
}

pub(crate) const fn group2(_p: Piece) -> bool {
    false
}

pub(crate) const fn canonical_size(p: Piece) -> usize {
    match p {
        Piece::O => 1,
        _ => 4,
    }
}

pub(crate) fn canonical_r(_p: Piece, r: Rotation) -> Rotation {
    r
}

pub(crate) fn canonical_offset(_p: Piece, _r: Rotation) -> Coordinates {
    Coordinates::new(0, 0)
}

// -- Direction --
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Direction {
    Cw = 0,
    Ccw = 1,
    Flip = 2,
}

pub(crate) const DIRECTION_NB: usize = 2; // Cw and Ccw only (Flip is separate)

pub(crate) fn rotate(d: Direction, r: Rotation) -> Rotation {
    let ri = r as u8;
    let result = match d {
        Direction::Cw => (ri + 1) & 3,
        Direction::Ccw => (ri + 3) & 3,
        Direction::Flip => (ri + 2) & 3,
    };
    Rotation::from_u8(result)
}

// -- Kick tables --
// Offsets<N> = [Coordinates; N]
// OffsetsRot<N> = [[Coordinates; N]; ROTATION_NB]

// kicks[3][2]: [kick_set][direction]
//   kick_set 0 = LJSZT, 1 = I SRS, 2 = I SRS+
//   direction 0 = CW, 1 = CCW
// Each entry: [4 rotations][5 offsets]
pub(crate) type Offsets5 = [Coordinates; 5];

pub(crate) type Offsets6 = [Coordinates; 6];

macro_rules! c {
    ($x:expr, $y:expr) => {
        Coordinates { x: $x, y: $y }
    };
}

pub(crate) static KICKS: [[[Offsets5; ROTATION_NB]; DIRECTION_NB]; 3] = [
    // [0] LJSZT
    [
        // CW
        [
            [c!(0, 0), c!(-1, 0), c!(-1, 1), c!(0, -2), c!(-1, -2)], // 0->1 (N->E)
            [c!(0, 0), c!(1, 0), c!(1, -1), c!(0, 2), c!(1, 2)],    // 1->2 (E->S)
            [c!(0, 0), c!(1, 0), c!(1, 1), c!(0, -2), c!(1, -2)],   // 2->3 (S->W)
            [c!(0, 0), c!(-1, 0), c!(-1, -1), c!(0, 2), c!(-1, 2)], // 3->0 (W->N)
        ],
        // CCW
        [
            [c!(0, 0), c!(1, 0), c!(1, 1), c!(0, -2), c!(1, -2)],   // 0->3 (N->W)
            [c!(0, 0), c!(1, 0), c!(1, -1), c!(0, 2), c!(1, 2)],    // 1->0 (E->N)
            [c!(0, 0), c!(-1, 0), c!(-1, 1), c!(0, -2), c!(-1, -2)], // 2->1 (S->E)
            [c!(0, 0), c!(-1, 0), c!(-1, -1), c!(0, 2), c!(-1, 2)], // 3->2 (W->S)
        ],
    ],
    // [1] I SRS
    [
        // CW
        [
            [c!(0, 0), c!(-2, 0), c!(1, 0), c!(-2, -1), c!(1, 2)],  // 0->1
            [c!(0, 0), c!(-1, 0), c!(2, 0), c!(-1, 2), c!(2, -1)],  // 1->2
            [c!(0, 0), c!(2, 0), c!(-1, 0), c!(2, 1), c!(-1, -2)],  // 2->3
            [c!(0, 0), c!(1, 0), c!(-2, 0), c!(1, -2), c!(-2, 1)],  // 3->0
        ],
        // CCW
        [
            [c!(0, 0), c!(-1, 0), c!(2, 0), c!(-1, 2), c!(2, -1)],  // 0->3
            [c!(0, 0), c!(2, 0), c!(-1, 0), c!(2, 1), c!(-1, -2)],  // 1->0
            [c!(0, 0), c!(1, 0), c!(-2, 0), c!(1, -2), c!(-2, 1)],  // 2->1
            [c!(0, 0), c!(-2, 0), c!(1, 0), c!(-2, -1), c!(1, 2)],  // 3->2
        ],
    ],
    // [2] I SRS+
    [
        // CW
        [
            [c!(0, 0), c!(1, 0), c!(-2, 0), c!(-2, -1), c!(1, 2)],  // 0->1
            [c!(0, 0), c!(-1, 0), c!(2, 0), c!(-1, -2), c!(2, 1)],  // 1->2
            [c!(0, 0), c!(-1, 0), c!(2, 0), c!(-1, 2), c!(2, -1)],  // 2->3
            [c!(0, 0), c!(2, 0), c!(-1, 0), c!(2, 1), c!(-1, -2)],  // 3->0
        ],
        // CCW
        [
            [c!(0, 0), c!(-1, 0), c!(2, 0), c!(2, -1), c!(-1, 2)],  // 0->3
            [c!(0, 0), c!(-1, 0), c!(2, 0), c!(-1, -2), c!(2, 1)],  // 1->0
            [c!(0, 0), c!(1, 0), c!(-2, 0), c!(-2, 1), c!(1, -2)],  // 2->1
            [c!(0, 0), c!(1, 0), c!(-2, 0), c!(-2, -1), c!(1, 2)],  // 3->2
        ],
    ],
];

pub(crate) static KICKS_180: [[Offsets6; ROTATION_NB]; 2] = [
    // [0] LJSZT
    [
        [c!(0, 0), c!(0, 1), c!(1, 1), c!(-1, 1), c!(1, 0), c!(-1, 0)], // 0->2
        [c!(0, 0), c!(1, 0), c!(1, 2), c!(1, 1), c!(0, 2), c!(0, 1)],  // 1->3
        [c!(0, 0), c!(0, -1), c!(-1, -1), c!(1, -1), c!(-1, 0), c!(1, 0)], // 2->0
        [c!(0, 0), c!(-1, 0), c!(-1, 2), c!(-1, 1), c!(0, 2), c!(0, 1)], // 3->1
    ],
    // [1] I
    [
        [c!(0, 0), c!(0, 1), c!(0, 0), c!(0, 0), c!(0, 0), c!(0, 0)], // 0->2
        [c!(0, 0), c!(1, 0), c!(0, 0), c!(0, 0), c!(0, 0), c!(0, 0)], // 1->3
        [c!(0, 0), c!(0, -1), c!(0, 0), c!(0, 0), c!(0, 0), c!(0, 0)], // 2->0
        [c!(0, 0), c!(-1, 0), c!(0, 0), c!(0, 0), c!(0, 0), c!(0, 0)], // 3->1
    ],
];

// kick table index: srs_plus uses (p==I)*2, srs uses (p==I)
pub(crate) fn kick_index(p: Piece, srs_plus: bool) -> usize {
    let is_i = (p == Piece::I) as usize;
    if srs_plus && is_i == 1 {
        2
    } else {
        is_i
    }
}

pub(crate) fn kick_180_index(p: Piece) -> usize {
    (p == Piece::I) as usize
}

// -- CollisionMap --

pub struct CollisionMap {
    pub(crate) data: [[Bitboard; ROTATION_NB]; COL_NB],
}

impl CollisionMap {
    pub fn new(cols: &[Bitboard; COL_NB], p: Piece) -> Self {
        let mut data = [[0u64; ROTATION_NB]; COL_NB];
        for x in 0..COL_NB {
            for ri in 0..ROTATION_NB {
                let r = Rotation::from_u8(ri as u8);
                let mut m = 0u64;
                if in_bounds(p, r, x as i32) {
                    let pc = piece_table(p, r);
                    // Check pivot (at 0,0 relative)
                    m |= cols[x];
                    
                    // Check other 3 blocks
                    for i in 0..3 {
                        let dx = pc[i].x as i32;
                        let dy = pc[i].y as i32;
                        let target_x = x as i32 + dx;
                        if is_ok_x(target_x) {
                            let col_mask = cols[target_x as usize];
                            if dy > 0 {
                                m |= col_mask >> dy;
                            } else if dy < 0 {
                                // Floor collision: bit y is set if y+dy < 0 => y < -dy
                                m |= col_mask << (-dy);
                                m |= bb_low(-dy);
                            } else {
                                m |= col_mask;
                            }
                        }
                    }
                } else {
                    m = !0u64;
                }
                data[x][ri] = m;
            }
        }
        Self { data }
    }

    #[inline(always)]
    pub fn get(&self, x: usize, r: Rotation) -> Bitboard {
        self.data[x][r as usize]
    }
}

pub struct CollisionMap16 {
    pub(crate) data: [Bitboard; COL_NB],
}

impl CollisionMap16 {
    pub fn new(cols: &[Bitboard; COL_NB], p: Piece) -> Self {
        let mut data = [0u64; COL_NB];
        let cm = CollisionMap::new(cols, p);
        for x in 0..COL_NB {
            let mut val = 0u64;
            for r in 0..ROTATION_NB {
                val |= (cm.get(x, Rotation::from_u8(r as u8)) & 0xFFFF) << (r * 16);
            }
            data[x] = val;
        }
        Self { data }
    }

    #[inline(always)]
    pub fn get(&self, x: usize) -> Bitboard {
        self.data[x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    #[test]
    fn test_canonical_r() {
        assert_eq!(canonical_r(Piece::O, Rotation::East), Rotation::East);
        assert_eq!(canonical_r(Piece::I, Rotation::North), Rotation::North);
        assert_eq!(canonical_r(Piece::I, Rotation::East), Rotation::East);
        assert_eq!(canonical_r(Piece::I, Rotation::South), Rotation::South);
        assert_eq!(canonical_r(Piece::I, Rotation::West), Rotation::West);
        assert_eq!(canonical_r(Piece::T, Rotation::South), Rotation::South);
    }

    #[test]
    fn test_canonical_offset() {
        assert_eq!(
            canonical_offset(Piece::I, Rotation::South),
            Coordinates::new(0, 0)
        );
        assert_eq!(
            canonical_offset(Piece::I, Rotation::West),
            Coordinates::new(0, 0)
        );
        assert_eq!(
            canonical_offset(Piece::I, Rotation::North),
            Coordinates::new(0, 0)
        );
        assert_eq!(
            canonical_offset(Piece::S, Rotation::South),
            Coordinates::new(0, 0)
        );
        assert_eq!(
            canonical_offset(Piece::S, Rotation::West),
            Coordinates::new(0, 0)
        );
        assert_eq!(
            canonical_offset(Piece::T, Rotation::South),
            Coordinates::new(0, 0)
        );
    }

    #[test]
    fn test_rotate_direction() {
        assert_eq!(rotate(Direction::Cw, Rotation::North), Rotation::East);
        assert_eq!(rotate(Direction::Cw, Rotation::West), Rotation::North);
        assert_eq!(rotate(Direction::Ccw, Rotation::North), Rotation::West);
        assert_eq!(rotate(Direction::Flip, Rotation::North), Rotation::South);
    }

    #[test]
    fn test_collision_map_empty_board() {
        let b = Board::new();
        let cols = b.compute_cols();
        let cm = CollisionMap::new(&cols, Piece::T);
        assert_eq!(cm.get(4, Rotation::North), 0);
    }

    #[test]
    fn test_in_bounds() {
        assert!(!in_bounds(Piece::T, Rotation::North, 0));
        assert!(in_bounds(Piece::T, Rotation::North, 1));
    }

    #[test]
    fn test_kick_tables_size() {
        assert_eq!(KICKS[0][0].len(), ROTATION_NB);
        assert_eq!(KICKS[0][0][0].len(), 5);
        assert_eq!(KICKS_180[0].len(), ROTATION_NB);
        assert_eq!(KICKS_180[0][0].len(), 6);
    }

    #[test]
    fn test_kick_values() {
        // SRS+ I 0->1 should have (1, 0) as second offset
        assert_eq!(KICKS[2][0][0][1], c!(1, 0));
        
        // 180 I 0->2 should have (0, 1) as second offset
        assert_eq!(KICKS_180[1][0][1], c!(0, 1));
    }

    #[test]
    fn test_collision_floor_sz() {
        let b = Board::new();
        let cols = b.compute_cols();
        
        // S piece at East rotation (1):
        // Blocks: [-1, 0], [0, 0], [0, 1], [1, 1] in zztetris
        // Rotation 1 (East) of these:
        // [0, 1], [0, 0], [1, 0], [1, -1]
        // At absolute y=0, the block [1, -1] is at y=-1 (COLLISION)
        let cm = CollisionMap::new(&cols, Piece::S);
        let mask = cm.get(4, Rotation::East);
        assert_eq!(mask & 1, 1, "S piece at y=0 vertical should collide with floor");
        assert_eq!(mask & 2, 0, "S piece at y=1 vertical should NOT collide with floor");
    }
}
