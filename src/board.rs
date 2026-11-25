//! 64-bit bitboard engine for 2048.
//!
//! Board layout: each tile occupies one 4-bit nibble. Nibble index `n = 4*r + c`
//! (row-major, starting at the low bits) holds the tile's *rank*: `0` = empty,
//! `k >= 1` = tile with value `2^k` (so the representable range is 2..=32768,
//! the same encoding used by all high-speed 2048 engines).
//!
//! All four moves are table-driven: a 65536-entry lookup table per direction maps
//! a 16-bit line (row or column) to its post-move value, and a second table maps
//! it to the score gained. This makes a full move 4 table lookups + a few ORs.

use std::sync::OnceLock;

/// Mask selecting one row (16 bits).
pub const ROW_MASK: u64 = 0xFFFF;
/// Mask selecting the low bit of every nibble (used for occupancy tests).
const NIBBLE_LSB_MASK: u64 = 0x1111_1111_1111_1111;

/// The four moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Move {
    Up,
    Down,
    Left,
    Right,
}

pub const MOVES: [Move; 4] = [Move::Up, Move::Left, Move::Down, Move::Right];

impl Move {
    #[inline]
    pub fn name(self) -> &'static str {
        match self {
            Move::Up => "UP",
            Move::Down => "DOWN",
            Move::Left => "LEFT",
            Move::Right => "RIGHT",
        }
    }

    pub fn from_name(s: &str) -> Option<Move> {
        match s.to_ascii_uppercase().as_str() {
            "U" | "UP" => Some(Move::Up),
            "D" | "DOWN" => Some(Move::Down),
            "L" | "LEFT" => Some(Move::Left),
            "R" | "RIGHT" => Some(Move::Right),
            _ => None,
        }
    }
}

/// Precomputed move / score / heuristic tables (built once, thread-safe).
pub struct Tables {
    /// Line after a LEFT move (nibble 0 is the left-most tile).
    pub line_left: [u16; 65536],
    /// Line after a RIGHT move (nibble 3 is the right-most tile).
    pub line_right: [u16; 65536],
    /// Score gained by moving this line (direction-invariant).
    pub line_score: [u32; 65536],
    /// Heuristic value of this line (see `line_heuristic`).
    pub line_heur: [f32; 65536],
}

static TABLES: OnceLock<Tables> = OnceLock::new();

/// Global precomputed tables, built on first use (thread-safe via `OnceLock`).
pub fn tables() -> &'static Tables {
    TABLES.get_or_init(build_tables)
}

fn build_tables() -> Tables {
    let mut t = Tables {
        line_left: [0; 65536],
        line_right: [0; 65536],
        line_score: [0; 65536],
        line_heur: [0.0; 65536],
    };
    for l in 0..65536usize {
        let line = l as u16;
        let (moved, score) = move_line_left(line);
        t.line_left[l] = moved;
        t.line_score[l] = score;
        t.line_right[l] = reverse_line(move_line_left(reverse_line(line)).0);
        t.line_heur[l] = line_heuristic(line);
    }
    t
}

/// Collapse one line towards nibble 0 (2048 rules: slide, merge each pair once,
/// score by the value of the produced tile). Returns `(new_line, score)`.
///
/// Merging two rank-15 tiles (32768 + 32768) would produce rank 16, which does
/// not fit in a nibble; the rank is capped at 15 but the score still counts the
/// true 65536 points. Capping only ever occurs once two 32768 tiles coexist,
/// which strong AIs essentially never reach (documented limitation, shared with
/// the classic reference engines).
fn move_line_left(line: u16) -> (u16, u32) {
    let ranks = [
        (line & 0xF) as u32,
        ((line >> 4) & 0xF) as u32,
        ((line >> 8) & 0xF) as u32,
        ((line >> 12) & 0xF) as u32,
    ];
    // Compact non-zero ranks to the front.
    let mut packed = [0u32; 4];
    let mut n = 0usize;
    for &r in &ranks {
        if r != 0 {
            packed[n] = r;
            n += 1;
        }
    }
    let mut out = [0u32; 4];
    let mut m = 0usize;
    let mut score = 0u32;
    let mut i = 0usize;
    while i < n {
        if i + 1 < n && packed[i] == packed[i + 1] {
            let merged_rank = packed[i] + 1;
            // True merged value (2^rank) even when the stored rank caps at 15.
            score += 1u32 << merged_rank;
            out[m] = merged_rank.min(15);
            m += 1;
            i += 2;
        } else {
            out[m] = packed[i];
            m += 1;
            i += 1;
        }
    }
    let mut result = 0u16;
    for (j, &r) in out.iter().enumerate() {
        result |= (r as u16) << (4 * j);
    }
    (result, score)
}

/// Mirror a 16-bit line (swap nibble 0 <-> 3 and 1 <-> 2).
#[inline]
fn reverse_line(l: u16) -> u16 {
    ((l & 0x000F) << 12) | ((l & 0x00F0) << 4) | ((l & 0x0F00) >> 4) | ((l & 0xF000) >> 12)
}

// ---------------------------------------------------------------------------
// Heuristic (weights follow the well-known tuned constants popularized by
// nneonneo's expectimax 2048 AI; see README for references).
// ---------------------------------------------------------------------------

const LOST_PENALTY: f32 = 200_000.0;
const EMPTY_WEIGHT: f32 = 270.0;
const MERGES_WEIGHT: f32 = 700.0;
const MONOTONICITY_POWER: f32 = 4.0;
const MONOTONICITY_WEIGHT: f32 = 47.0;
const SUM_POWER: f32 = 3.5;
const SUM_WEIGHT: f32 = 11.0;

fn powf_table(power: f32) -> [f32; 16] {
    let mut t = [0.0f32; 16];
    for (r, v) in t.iter_mut().enumerate() {
        *v = (r as f32).powf(power);
    }
    t
}

/// Heuristic value of a single line: rewards empty cells and potential merges,
/// penalises non-monotonic lines and large scattered ranks.
fn line_heuristic(line: u16) -> f32 {
    static P4: OnceLock<[f32; 16]> = OnceLock::new();
    static P35: OnceLock<[f32; 16]> = OnceLock::new();
    let p4 = P4.get_or_init(|| powf_table(MONOTONICITY_POWER));
    let p35 = P35.get_or_init(|| powf_table(SUM_POWER));

    let ranks = [
        (line & 0xF) as usize,
        ((line >> 4) & 0xF) as usize,
        ((line >> 8) & 0xF) as usize,
        ((line >> 12) & 0xF) as usize,
    ];

    let mut empty = 0.0f32;
    let mut sum = 0.0f32;
    let mut merges = 0.0f32;
    let mut prev = 0usize;
    let mut counter = 0usize;
    for &r in &ranks {
        sum += p35[r];
        if r == 0 {
            empty += 1.0;
        } else {
            if prev == r {
                counter += 1;
            } else if counter > 0 {
                merges += (1 + counter) as f32;
                counter = 0;
            }
            prev = r;
        }
    }
    if counter > 0 {
        merges += (1 + counter) as f32;
    }

    let mut mono_left = 0.0f32;
    let mut mono_right = 0.0f32;
    for i in 1..4 {
        if ranks[i - 1] > ranks[i] {
            mono_left += p4[ranks[i - 1]] - p4[ranks[i]];
        } else {
            mono_right += p4[ranks[i]] - p4[ranks[i - 1]];
        }
    }

    LOST_PENALTY + EMPTY_WEIGHT * empty + MERGES_WEIGHT * merges
        - MONOTONICITY_WEIGHT * mono_left.min(mono_right)
        - SUM_WEIGHT * sum
}

// ---------------------------------------------------------------------------
// Board primitives
// ---------------------------------------------------------------------------

/// Transpose the board (swap rows and columns) using the classic bit trick.
#[inline]
pub fn transpose(x: u64) -> u64 {
    let a1 = x & 0xF0F0_0F0F_F0F0_0F0F;
    let a2 = x & 0x0000_F0F0_0000_F0F0;
    let a3 = x & 0x0F0F_0000_0F0F_0000;
    let a = a1 | (a2 << 12) | (a3 >> 12);
    let b1 = a & 0xFF00_FF00_00FF_00FF;
    let b2 = a & 0x00FF_00FF_0000_0000;
    let b3 = a & 0x0000_0000_FF00_FF00;
    b1 | (b2 >> 24) | (b3 << 24)
}

/// Read column `c` (0 = left-most) as a compact 16-bit line, nibble 0 = row 0.
#[inline]
pub fn get_col(b: u64, c: usize) -> u16 {
    let s = 4 * c;
    let n0 = (b >> s) & 0xF;
    let n1 = (b >> (16 + s)) & 0xF;
    let n2 = (b >> (32 + s)) & 0xF;
    let n3 = (b >> (48 + s)) & 0xF;
    (n0 | (n1 << 4) | (n2 << 8) | (n3 << 12)) as u16
}

/// Inverse of `get_col`: place a compact 16-bit column back into column `c`.
#[inline]
fn scatter_col(v: u16, c: usize) -> u64 {
    let s = 4 * c;
    let n0 = (v & 0xF) as u64;
    let n1 = ((v >> 4) & 0xF) as u64;
    let n2 = ((v >> 8) & 0xF) as u64;
    let n3 = ((v >> 12) & 0xF) as u64;
    (n0 << s) | (n1 << (16 + s)) | (n2 << (32 + s)) | (n3 << (48 + s))
}

/// Execute a move. Returns `(new_board, score_gained)`; if the move does not
/// change the board, `new_board == board` and the score is 0.
#[inline]
pub fn execute_move(m: Move, b: u64) -> (u64, u32) {
    let t = tables();
    match m {
        Move::Left => line_move(b, &t.line_left),
        Move::Right => line_move(b, &t.line_right),
        Move::Up => col_move(b, &t.line_left),
        Move::Down => col_move(b, &t.line_right),
    }
}

#[inline]
fn line_move(b: u64, table: &[u16; 65536]) -> (u64, u32) {
    let t = tables();
    let mut nb = 0u64;
    let mut score = 0u32;
    for r in 0..4 {
        let line = ((b >> (16 * r)) & ROW_MASK) as u16;
        nb |= u64::from(table[line as usize]) << (16 * r);
        score += t.line_score[line as usize];
    }
    (nb, score)
}

#[inline]
fn col_move(b: u64, table: &[u16; 65536]) -> (u64, u32) {
    let t = tables();
    let mut nb = 0u64;
    let mut score = 0u32;
    for c in 0..4 {
        let line = get_col(b, c);
        nb |= scatter_col(table[line as usize], c);
        score += t.line_score[line as usize];
    }
    (nb, score)
}

/// Bitmask whose set bits are the low bits of empty nibbles.
#[inline]
pub fn empty_mask(b: u64) -> u64 {
    NIBBLE_LSB_MASK & !((b | (b >> 1) | (b >> 2) | (b >> 3)) & NIBBLE_LSB_MASK)
}

#[inline]
pub fn count_empty(b: u64) -> u32 {
    empty_mask(b).count_ones()
}

/// Number of distinct non-zero ranks on the board.
#[inline]
pub fn count_distinct(b: u64) -> u32 {
    let mut seen = 0u32;
    for i in 0..16 {
        let r = ((b >> (4 * i)) & 0xF) as u32;
        if r > 0 {
            seen |= 1 << r;
        }
    }
    seen.count_ones()
}

/// Highest tile value on the board (0 for an empty board).
#[inline]
pub fn max_tile_value(b: u64) -> u32 {
    let mut best = 0u32;
    for i in 0..16 {
        let r = ((b >> (4 * i)) & 0xF) as u32;
        if r > 0 {
            best = best.max(1 << r);
        }
    }
    best
}

/// True if no move changes the board (game over).
#[inline]
pub fn is_dead(b: u64) -> bool {
    MOVES.iter().all(|&m| execute_move(m, b).0 == b)
}

/// Evaluate the board with the row+column heuristic.
#[inline]
pub fn evaluate(b: u64) -> f32 {
    let t = tables();
    let mut s = 0.0f32;
    for r in 0..4 {
        s += t.line_heur[((b >> (16 * r)) & ROW_MASK) as usize];
    }
    for c in 0..4 {
        s += t.line_heur[get_col(b, c) as usize];
    }
    s
}

/// Build a board from 16 displayed values (row-major, 0 = empty).
/// Values must be powers of two in 2..=32768.
pub fn board_from_values(vals: &[u32; 16]) -> Result<u64, String> {
    let mut b = 0u64;
    for (i, &v) in vals.iter().enumerate() {
        if v == 0 {
            continue;
        }
        if !v.is_power_of_two() {
            return Err(format!("tile value {v} is not a power of two"));
        }
        let r = v.trailing_zeros();
        if !(1..=15).contains(&r) {
            return Err(format!("tile value {v} out of supported range 2..=32768"));
        }
        b |= u64::from(r) << (4 * i);
    }
    Ok(b)
}

/// Inverse of `board_from_values`.
pub fn board_to_values(b: u64) -> [u32; 16] {
    let mut vals = [0u32; 16];
    for (i, v) in vals.iter_mut().enumerate() {
        let r = (b >> (4 * i)) & 0xF;
        *v = if r == 0 { 0 } else { 1u32 << r };
    }
    vals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ranks: [u32; 4]) -> u16 {
        let mut l = 0u16;
        for (i, &r) in ranks.iter().enumerate() {
            l |= (r as u16) << (4 * i);
        }
        l
    }

    #[test]
    fn move_line_left_basics() {
        // [2,2,2,2] -> [4,4,_,_] score 8
        let (m, s) = move_line_left(line([1, 1, 1, 1]));
        assert_eq!(m, line([2, 2, 0, 0]));
        assert_eq!(s, 8);
        // [4,4,8,8] -> [8,16,_,_] score 24
        let (m, s) = move_line_left(line([2, 2, 3, 3]));
        assert_eq!(m, line([3, 4, 0, 0]));
        assert_eq!(s, 24);
        // [2,_,2,4] -> [4,4,_,_] score 4
        let (m, s) = move_line_left(line([1, 0, 1, 2]));
        assert_eq!(m, line([2, 2, 0, 0]));
        assert_eq!(s, 4);
        // [4,4,4,_] -> [8,4,_,_] score 8 (each tile merges at most once)
        let (m, s) = move_line_left(line([2, 2, 2, 0]));
        assert_eq!(m, line([3, 2, 0, 0]));
        assert_eq!(s, 8);
        // [_,2,_,4] -> [2,4,_,_] score 0
        let (m, s) = move_line_left(line([0, 1, 0, 2]));
        assert_eq!(m, line([1, 2, 0, 0]));
        assert_eq!(s, 0);
    }

    #[test]
    fn rank15_merge_scores_65536_but_caps() {
        // two 32768 tiles merge: score counts 65536, rank caps at 15
        let (m, s) = move_line_left(line([15, 15, 0, 0]));
        assert_eq!(m, line([15, 0, 0, 0]));
        assert_eq!(s, 65_536);
    }

    #[test]
    fn score_is_direction_invariant() {
        // Moving a line left or right must produce the same score.
        let t = tables();
        let mut rng: u64 = 0x243F_6A88_85A3_08D3;
        for _ in 0..2000 {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let l = (rng >> 48) as u16;
            assert_eq!(
                t.line_score[l as usize],
                t.line_score[reverse_line(l) as usize]
            );
        }
    }

    #[test]
    fn transpose_matches_naive() {
        let naive = |b: u64| {
            let mut t = 0u64;
            for r in 0..4 {
                for c in 0..4 {
                    let v = (b >> (4 * (4 * r + c))) & 0xF;
                    t |= v << (4 * (4 * c + r));
                }
            }
            t
        };
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..2000 {
            rng = rng
                .wrapping_mul(2862933555777941757)
                .wrapping_add(3037000493);
            let b = rng;
            assert_eq!(transpose(b), naive(b));
        }
        assert_eq!(transpose(0), 0);
    }

    #[test]
    fn get_col_scatter_roundtrip() {
        let mut rng: u64 = 0xDEAD_BEEF_CAFE_1234;
        for _ in 0..2000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            for c in 0..4 {
                let col = get_col(rng, c);
                assert_eq!(get_col(scatter_col(col, c), c), col);
            }
        }
    }

    #[test]
    fn execute_move_up_down_left_right() {
        // Board:
        //  2  0  0  0
        //  2  0  0  0
        //  4  0  0  0
        //  0  0  0  0
        let b = board_from_values(&[
            2, 0, 0, 0, //
            2, 0, 0, 0, //
            4, 0, 0, 0, //
            0, 0, 0, 0,
        ])
        .unwrap();
        // UP: col0 = [2,2,4,_] -> compact [2,2,4], merge 2+2 -> 4 =>
        // [4,4,_,_] score 4 (only the 2+2 merge scores; the produced 4 and
        // the original 4 do not merge again).
        let (up, s) = execute_move(Move::Up, b);
        let expect_up = board_from_values(&[
            4, 0, 0, 0, //
            4, 0, 0, 0, //
            0, 0, 0, 0, //
            0, 0, 0, 0,
        ])
        .unwrap();
        assert_eq!(up, expect_up);
        assert_eq!(s, 4);
        // DOWN: col0 = [2,2,4,_] -> ranks compact to bottom: [2,2,4] -> merge
        // 2+2 at second-from-bottom: [_,_,4,4] score 4.
        let (down, s) = execute_move(Move::Down, b);
        let expect_down = board_from_values(&[
            0, 0, 0, 0, //
            0, 0, 0, 0, //
            4, 0, 0, 0, //
            4, 0, 0, 0,
        ])
        .unwrap();
        assert_eq!(down, expect_down);
        assert_eq!(s, 4);
        // LEFT: each row is a single tile already left-packed => no-op.
        let (left, s) = execute_move(Move::Left, b);
        assert_eq!(left, b);
        assert_eq!(s, 0);
    }

    #[test]
    fn board_values_roundtrip_and_validation() {
        let vals = [
            2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 0,
        ];
        let b = board_from_values(&vals).unwrap();
        assert_eq!(board_to_values(b), vals);
        assert_eq!(max_tile_value(b), 32768);
        assert!(board_from_values(&[3; 16]).is_err());
        assert!(board_from_values(&[65536, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(board_from_values(&[0; 16]).is_ok());
    }

    #[test]
    fn empty_and_distinct_counts() {
        let b = board_from_values(&[
            2, 2, 4, 0, //
            8, 0, 0, 0, //
            8, 16, 0, 0, //
            0, 0, 0, 0,
        ])
        .unwrap();
        assert_eq!(count_empty(b), 10);
        assert_eq!(count_distinct(b), 4); // ranks 1,2,3,4
        assert_eq!(empty_mask(b).count_ones(), 10);
    }

    #[test]
    fn is_dead_detection() {
        // Full alternating board: dead.
        let dead = board_from_values(&[
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4, //
            4, 2, 4, 2,
        ])
        .unwrap();
        assert!(is_dead(dead));
        // Same board with one merge available: alive.
        let alive = board_from_values(&[
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4, //
            4, 2, 2, 2,
        ])
        .unwrap();
        assert!(!is_dead(alive));
        // Any playable board with a movable tile is alive.
        let lone = board_from_values(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert!(!is_dead(lone));
        // An empty board is vacuously "dead" under the no-move-changes-board
        // definition; it never occurs in real play (games start with 2 tiles).
        assert!(is_dead(0));
    }

    #[test]
    fn heuristic_prefers_monotone_and_empty() {
        // A monotone snake line should evaluate better than a scrambled one.
        let snake = line([12, 11, 10, 9]);
        let scrambled = line([9, 12, 10, 11]);
        let t = tables();
        assert!(t.line_heur[snake as usize] > t.line_heur[scrambled as usize]);
        // More empty cells is better for the same tile multiset.
        let sparse = line([8, 0, 0, 0]);
        let dense = line([0, 0, 1, 8]);
        assert!(t.line_heur[sparse as usize] > t.line_heur[dense as usize]);
    }
}
