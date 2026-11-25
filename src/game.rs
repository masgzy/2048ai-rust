//! Game state, seeded RNG and the automated play loop.

use std::time::Instant;

use crate::ai::Expectimax;
use crate::board::{self, Move};

/// PCG32 (Permuted Congruential Generator) — small, fast, well-tested.
/// Seeded games are exactly reproducible, which the regression tests rely on.
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    pub fn new(seed: u64) -> Self {
        let mut rng = Pcg32 {
            state: 0,
            inc: (seed << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(0x853C_49E6_748F_EA9B);
        rng.next_u32();
        rng
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6364136223846793005)
            .wrapping_add(self.inc | 1);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }
}

/// A single 2048 game.
pub struct Game {
    pub board: u64,
    pub score: u32,
    pub moves: u64,
    /// Highest tile value seen so far.
    pub max_tile: u32,
    pub over: bool,
    rng: Pcg32,
}

/// Result of one completed (or goal-truncated) game.
#[derive(Clone, Copy, Debug)]
pub struct GameReport {
    pub seed: u64,
    pub score: u32,
    pub max_tile: u32,
    pub moves: u64,
    pub nodes: u64,
    pub duration_secs: f64,
    /// Whether a goal tile was requested and reached.
    pub reached_goal: Option<bool>,
    pub over: bool,
}

impl Game {
    pub fn new(seed: u64) -> Self {
        let mut g = Game {
            board: 0,
            score: 0,
            moves: 0,
            max_tile: 0,
            over: false,
            rng: Pcg32::new(seed),
        };
        g.spawn_tile();
        g.spawn_tile();
        g
    }

    /// Place a random tile: 2 with probability 0.9, 4 with probability 0.1.
    ///
    /// The modulo reduction over a u32 stream has a bias of less than 2^-29,
    /// which is negligible for gameplay (and constant, so seeded runs stay
    /// reproducible).
    fn spawn_tile(&mut self) {
        let m = board::empty_mask(self.board);
        debug_assert!(m != 0, "spawn on a full board");
        if m == 0 {
            self.over = true;
            return;
        }
        let n = m.count_ones();
        let pick = self.rng.next_u32() % n;
        let mut mm = m;
        for _ in 0..pick {
            mm &= mm - 1;
        }
        let shift = mm.trailing_zeros();
        let rank = if self.rng.next_u32().is_multiple_of(10) {
            2u64
        } else {
            1u64
        };
        self.board |= rank << shift;
        let value = if rank == 2 { 4 } else { 2 };
        self.max_tile = self.max_tile.max(value);
    }

    /// Apply a move; returns false (and changes nothing) if it is a no-op.
    pub fn apply_move(&mut self, m: Move) -> bool {
        let (nb, sc) = board::execute_move(m, self.board);
        if nb == self.board {
            return false;
        }
        self.board = nb;
        self.score = self.score.wrapping_add(sc);
        self.moves += 1;
        self.max_tile = self.max_tile.max(board::max_tile_value(nb));
        true
    }

    /// Play the full game automatically. `goal` (a tile value) truncates the
    /// game once reached. `on_step` is invoked after every move for rendering.
    pub fn play_ai(
        &mut self,
        ai: &mut Expectimax,
        goal: Option<u32>,
        max_moves: u64,
        mut on_step: impl FnMut(&Game),
    ) -> GameReport {
        let start = Instant::now();
        loop {
            if let Some(g) = goal {
                if self.max_tile >= g {
                    break;
                }
            }
            if self.moves >= max_moves {
                break;
            }
            let Some((mv, _)) = ai.best_move(self.board) else {
                self.over = true;
                break;
            };
            let ok = self.apply_move(mv);
            debug_assert!(ok, "AI chose an illegal move");
            if !ok {
                self.over = true;
                break;
            }
            self.spawn_tile();
            on_step(self);
            if board::count_empty(self.board) == 0 && board::is_dead(self.board) {
                self.over = true;
                break;
            }
        }
        GameReport {
            seed: 0, // filled by caller
            score: self.score,
            max_tile: self.max_tile,
            moves: self.moves,
            nodes: ai.nodes,
            duration_secs: start.elapsed().as_secs_f64(),
            reached_goal: goal.map(|g| self.max_tile >= g),
            over: self.over,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_game_prefix() {
        let mut g1 = Game::new(12345);
        let mut g2 = Game::new(12345);
        let mut ai1 = Expectimax::new(2).unwrap();
        let mut ai2 = Expectimax::new(2).unwrap();
        let r1 = g1.play_ai(&mut ai1, None, 60, |_| {});
        let r2 = g2.play_ai(&mut ai2, None, 60, |_| {});
        assert_eq!(r1.moves, r2.moves);
        assert_eq!(r1.score, r2.score);
        assert_eq!(g1.board, g2.board);
    }

    #[test]
    fn spawn_only_on_empty_cells_and_distribution() {
        let mut g = Game::new(7);
        // Force a dense board.
        g.board = crate::board::board_from_values(&[
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4, //
            4, 2, 4, 0,
        ])
        .unwrap();
        g.max_tile = 4;
        g.score = 0;
        g.moves = 0;
        let mut twos = 0u32;
        let mut fours = 0u32;
        for _ in 0..1000 {
            g.board = crate::board::board_from_values(&[
                2, 4, 2, 4, //
                4, 2, 4, 2, //
                2, 4, 2, 4, //
                4, 2, 4, 0,
            ])
            .unwrap();
            let before = g.board;
            g.spawn_tile();
            let diff = g.board ^ before;
            // exactly the previously-empty nibble became non-zero
            assert_eq!(
                diff.count_ones(),
                g.board.count_ones() - before.count_ones()
            );
            assert!(diff != 0);
            let new_rank = (g.board >> 60) & 0xF;
            assert!(new_rank == 1 || new_rank == 2);
            if new_rank == 1 {
                twos += 1;
            } else {
                fours += 1;
            }
        }
        // ~10% fours with generous bounds (seeded, deterministic).
        assert!((60..=160).contains(&fours), "fours={fours}");
        assert_eq!(twos + fours, 1000);
    }

    #[test]
    fn dead_game_ends() {
        let mut g = Game::new(99);
        g.board = crate::board::board_from_values(&[
            2, 4, 2, 4, //
            4, 2, 4, 2, //
            2, 4, 2, 4, //
            4, 2, 4, 2,
        ])
        .unwrap();
        g.max_tile = 4;
        let mut ai = Expectimax::new(1).unwrap();
        let rep = g.play_ai(&mut ai, None, 100, |_| {});
        assert!(rep.over);
        assert_eq!(rep.moves, 0);
    }
}
