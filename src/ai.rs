//! Expectimax search with transposition table and probabilistic pruning.
//!
//! The search alternates:
//! - **max nodes** (our move): try all 4 moves, keep the best expected value;
//! - **chance nodes** (random spawn): for every empty cell, average the value of
//!   spawning a 2 (probability 0.9) and a 4 (probability 0.1).
//!
//! Branches whose cumulative probability drops below `cprob_min` are cut off and
//! evaluated heuristically, which keeps the expectimax tree tractable without
//! materially hurting move quality (the technique used by the classic
//! nneonneo 2048-ai; see README references).
//!
//! Concurrency: a single `Expectimax` instance is *not* `Sync` by intent — the
//! transposition table is plain mutable state. The benchmark harness therefore
//! gives each worker thread its own instance (no shared mutable search state,
//! no locks, no data races; verified by `tests/concurrency.rs`).

use crate::board::{self, Move, MOVES};

/// Search strength preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strength {
    pub min_depth: u32,
    pub max_depth: u32,
    pub cprob_min: f32,
}

/// Tuned presets, level 1 (fastest) ..= 7 (deepest).
///
/// The probability cutoff is the main cost knob: chance nodes multiply the
/// path probability by roughly `0.9 / empty_cells`, so dense boards (cheap
/// branching) are searched deeply while sparse boards (expensive branching)
/// cut off early — exactly the adaptivity an expectimax 2048 engine wants.
pub const STRENGTH_LEVELS: [Strength; 7] = [
    Strength {
        min_depth: 2,
        max_depth: 3,
        cprob_min: 1e-2,
    }, // 1 fast (~0.1 ms/move)
    Strength {
        min_depth: 2,
        max_depth: 4,
        cprob_min: 5e-3,
    }, // 2      (~1 ms/move)
    Strength {
        min_depth: 3,
        max_depth: 5,
        cprob_min: 2e-3,
    }, // 3      (~10 ms/move)
    Strength {
        min_depth: 3,
        max_depth: 6,
        cprob_min: 1e-3,
    }, // 4      (~30 ms/move)
    Strength {
        min_depth: 3,
        max_depth: 7,
        cprob_min: 5e-4,
    }, // 5      (~100 ms/move)
    Strength {
        min_depth: 4,
        max_depth: 9,
        cprob_min: 2e-4,
    }, // 6      (~300 ms/move)
    Strength {
        min_depth: 4,
        max_depth: 12,
        cprob_min: 1e-4,
    }, // 7 deep (~1 s/move)
];

impl Strength {
    pub fn from_level(level: u8) -> Result<Strength, String> {
        if !(1..=7).contains(&level) {
            return Err(format!("strength must be 1..=7, got {level}"));
        }
        Ok(STRENGTH_LEVELS[(level - 1) as usize])
    }
}

#[derive(Clone, Copy)]
struct TtEntry {
    key: u64,
    val: f32,
    depth: u8,
}

/// Transposition table size: 2^22 entries x 16 bytes = 64 MiB per instance.
const TT_BITS: u32 = 22;

pub struct Expectimax {
    strength: Strength,
    tt: Vec<TtEntry>,
    tt_mask: usize,
    /// Visited chance nodes (the "throughput" metric reported by benchmarks).
    pub nodes: u64,
    pub tt_hits: u64,
    pub tt_stores: u64,
    pub last_depth: u32,
}

impl Expectimax {
    pub fn new(level: u8) -> Result<Self, String> {
        Ok(Expectimax {
            strength: Strength::from_level(level)?,
            tt: vec![
                TtEntry {
                    key: 0,
                    val: 0.0,
                    depth: 0,
                };
                1usize << TT_BITS
            ],
            tt_mask: (1usize << TT_BITS) - 1,
            nodes: 0,
            tt_hits: 0,
            tt_stores: 0,
            last_depth: 0,
        })
    }

    pub fn strength(&self) -> Strength {
        self.strength
    }

    /// Clear the transposition table (e.g. between games). ~4M entries.
    pub fn clear_tt(&mut self) {
        self.tt.fill(TtEntry {
            key: 0,
            val: 0.0,
            depth: 0,
        });
        self.nodes = 0;
        self.tt_hits = 0;
        self.tt_stores = 0;
    }

    /// Pick the best move for `board`; `None` if the game is over.
    ///
    /// Fully deterministic for a fixed board + table state (no timers, no
    /// randomness), which makes seeded games reproducible.
    pub fn best_move(&mut self, board: u64) -> Option<(Move, f32)> {
        let depth = self.calc_depth(board);
        self.last_depth = depth;
        let mut best: Option<(Move, f32)> = None;
        for &m in MOVES.iter() {
            let (nb, _) = board::execute_move(m, board);
            if nb == board {
                continue;
            }
            let v = self.chance_node(nb, 1.0, depth);
            if best.is_none_or(|(_, bv)| v > bv) {
                best = Some((m, v));
            }
        }
        best
    }

    /// Adaptive depth: deeper when the board is complex (many distinct ranks)
    /// or crowded (few empty cells), clamped to the strength preset.
    fn calc_depth(&self, board: u64) -> u32 {
        let distinct = board::count_distinct(board);
        let empties = board::count_empty(board);
        let mut d = self.strength.min_depth.max(distinct.saturating_sub(2));
        if empties <= 4 {
            d += 1;
        }
        if empties <= 2 {
            d += 1;
        }
        if empties == 0 {
            d += 1;
        }
        d.min(self.strength.max_depth)
    }

    fn chance_node(&mut self, board: u64, cprob: f32, depth: u32) -> f32 {
        self.nodes += 1;
        if depth == 0 || cprob < self.strength.cprob_min {
            return board::evaluate(board);
        }
        if let Some(v) = self.tt_get(board, depth) {
            self.tt_hits += 1;
            return v;
        }
        let mut m = board::empty_mask(board);
        // A chance node is only reached after a valid move, which always leaves
        // at least one empty cell; the guard keeps the function total anyway.
        if m == 0 {
            return board::evaluate(board);
        }
        let num = m.count_ones() as f32;
        let inv = 1.0 / num;
        let mut sum = 0.0f32;
        while m != 0 {
            let shift = m.trailing_zeros();
            m &= m - 1;
            let board_two = board | (1u64 << shift);
            let board_four = board | (2u64 << shift);
            sum += 0.9 * self.max_node(board_two, cprob * 0.9 * inv, depth);
            sum += 0.1 * self.max_node(board_four, cprob * 0.1 * inv, depth);
        }
        let v = sum * inv;
        self.tt_stores += 1;
        self.tt_put(board, depth, v);
        v
    }

    fn max_node(&mut self, board: u64, cprob: f32, depth: u32) -> f32 {
        let mut best = 0.0f32;
        for &m in MOVES.iter() {
            let (nb, _) = board::execute_move(m, board);
            if nb != board {
                let v = self.chance_node(nb, cprob, depth - 1);
                if v > best {
                    best = v;
                }
            }
        }
        // best == 0.0 only when the board is dead; the heuristic baseline is
        // ~1.6M so dead lines compare as catastrophically bad.
        best
    }

    #[inline]
    fn tt_index(&self, board: u64) -> usize {
        // The board itself is a perfect hash; scramble it to spread collisions.
        board.wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize & self.tt_mask
    }

    #[inline]
    fn tt_get(&self, board: u64, depth: u32) -> Option<f32> {
        let e = self.tt[self.tt_index(board)];
        // Entry unused when key == 0; a real all-zero board is never searched
        // (games always have >= 2 tiles), so key == 0 doubles as "empty".
        if e.key == board && u32::from(e.depth) >= depth {
            Some(e.val)
        } else {
            None
        }
    }

    #[inline]
    fn tt_put(&mut self, board: u64, depth: u32, val: f32) {
        let idx = self.tt_index(board);
        let e = &self.tt[idx];
        // Keep a deeper existing entry for the same board if present.
        if e.key == board && u32::from(e.depth) > depth {
            return;
        }
        self.tt[idx] = TtEntry {
            key: board,
            val,
            depth: depth as u8,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::board_from_values;

    #[test]
    fn strength_levels_validate() {
        assert!(Strength::from_level(0).is_err());
        assert!(Strength::from_level(8).is_err());
        for lvl in 1..=7 {
            let s = Strength::from_level(lvl).unwrap();
            assert!(s.min_depth >= 2 && s.min_depth <= s.max_depth);
        }
    }

    #[test]
    fn best_move_is_always_legal() {
        let mut ai = Expectimax::new(3).unwrap();
        let mut rng: u64 = 0x1234_5678_9ABC_DEF0;
        for _ in 0..200 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let b = rng & 0x0F0F_0F0F_0F0F_0F0F; // valid rank range
            if let Some((m, _)) = ai.best_move(b) {
                let (nb, _) = board::execute_move(m, b);
                assert_ne!(nb, b, "AI returned a no-op move");
            }
        }
    }

    #[test]
    fn dead_board_has_no_move() {
        let mut ai = Expectimax::new(3).unwrap();
        let dead = board_from_values(&[
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4, //
            4, 2, 4, 2,
        ])
        .unwrap();
        assert!(ai.best_move(dead).is_none());
    }

    #[test]
    fn picks_the_only_legal_move_in_all_four_directions() {
        // Boards engineered so that exactly one direction changes anything;
        // any sane search must pick it at any strength.
        let only_right = board_from_values(&[
            2, 4, 2, 0, //
            4, 2, 4, 0, //
            2, 4, 2, 0, //
            4, 2, 4, 0,
        ])
        .unwrap();
        let only_left = board_from_values(&[
            0, 2, 4, 2, //
            0, 4, 2, 4, //
            0, 2, 4, 2, //
            0, 4, 2, 4,
        ])
        .unwrap();
        let only_up = board_from_values(&[
            0, 0, 0, 0, //
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4,
        ])
        .unwrap();
        let only_down = board_from_values(&[
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4, //
            0, 0, 0, 0,
        ])
        .unwrap();
        for level in 1..=7 {
            let mut ai = Expectimax::new(level).unwrap();
            assert_eq!(ai.best_move(only_right).unwrap().0, Move::Right);
            assert_eq!(ai.best_move(only_left).unwrap().0, Move::Left);
            assert_eq!(ai.best_move(only_up).unwrap().0, Move::Up);
            assert_eq!(ai.best_move(only_down).unwrap().0, Move::Down);
        }
    }

    #[test]
    fn deep_search_prefers_building_the_snake() {
        // A nearly-perfect snake; the sensible continuation is RIGHT or DOWN
        // (feeding the 2/4 tail), never UP (which rips tiles off the anchored
        // 1024 corner row).
        let b = board_from_values(&[
            1024, 512, 256, 128, //
            8, 16, 32, 64, //
            4, 2, 8, 2, //
            2, 0, 4, 0,
        ])
        .unwrap();
        let mut ai = Expectimax::new(5).unwrap();
        let (m, _) = ai.best_move(b).unwrap();
        assert!(
            m == Move::Down || m == Move::Right,
            "unexpected move {}",
            m.name()
        );
    }

    #[test]
    fn search_is_deterministic_with_tt() {
        let b = board_from_values(&[
            512, 256, 128, 64, //
            32, 16, 8, 4, //
            2, 2, 0, 0, //
            0, 0, 0, 0,
        ])
        .unwrap();
        let mut a1 = Expectimax::new(4).unwrap();
        let mut a2 = Expectimax::new(4).unwrap();
        let r1 = a1.best_move(b);
        let r2 = a2.best_move(b);
        assert_eq!(r1, r2);
        // Second call on the same board hits the TT and must agree.
        let r1b = a1.best_move(b);
        assert_eq!(r1, r1b);
        assert!(a1.tt_hits > 0 || a1.nodes == 0);
    }

    #[test]
    fn adaptive_depth_respects_bounds() {
        let mut ai = Expectimax::new(3).unwrap();
        // Nearly-empty board: depth = min_depth (3).
        let sparse = board_from_values(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4]).unwrap();
        ai.best_move(sparse).unwrap();
        assert_eq!(ai.last_depth, 3);
        // Crowded complex board: clamped to max_depth (5).
        let dense = board_from_values(&[
            4096, 2048, 1024, 512, //
            256, 128, 64, 32, //
            16, 8, 4, 2, //
            2, 4, 8, 0,
        ])
        .unwrap();
        ai.best_move(dense).unwrap();
        assert_eq!(ai.last_depth, 5);
    }
}
