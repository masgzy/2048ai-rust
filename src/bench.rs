//! Automated multi-game benchmark and raw search-throughput measurement.
//!
//! Concurrency model (race-free by construction):
//! - an `AtomicUsize` hands out game indices (each index is claimed exactly
//!   once, so no two threads ever play the same game);
//! - every worker owns its `Game` + `Expectimax` (including its transposition
//!   table) — there is no shared mutable search state;
//! - results flow over an `mpsc` channel to the single printing thread;
//! - no `unsafe`, no locks on hot paths.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use crate::ai::Expectimax;
use crate::game::{Game, GameReport};

pub struct BenchConfig {
    pub games: usize,
    pub threads: usize,
    pub strength: u8,
    pub seed_base: u64,
    pub goal: Option<u32>,
    pub max_moves: u64,
    pub quiet: bool,
}

impl Default for BenchConfig {
    fn default() -> Self {
        BenchConfig {
            games: 8,
            threads: thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            strength: 4,
            seed_base: 1,
            goal: None,
            max_moves: 200_000,
            quiet: false,
        }
    }
}

/// Aggregated statistics over all played games.
pub struct BenchSummary {
    pub games: usize,
    pub total_moves: u64,
    pub total_nodes: u64,
    pub avg_score: f64,
    pub best_score: u32,
    pub avg_max_tile: f64,
    pub best_max_tile: u32,
    pub reached_2048: usize,
    pub reached_4096: usize,
    pub reached_8192: usize,
    pub reached_16384: usize,
    pub reached_goal: usize,
    pub wall_secs: f64,
}

impl BenchSummary {
    pub fn moves_per_sec(&self) -> f64 {
        self.total_moves as f64 / self.wall_secs
    }
    pub fn nodes_per_sec(&self) -> f64 {
        self.total_nodes as f64 / self.wall_secs
    }
    pub fn games_per_min(&self) -> f64 {
        self.games as f64 / self.wall_secs * 60.0
    }
}

fn summarize(reports: &[GameReport], wall_secs: f64) -> BenchSummary {
    let games = reports.len();
    let total_moves: u64 = reports.iter().map(|r| r.moves).sum();
    let total_nodes: u64 = reports.iter().map(|r| r.nodes).sum();
    let avg_score = reports.iter().map(|r| r.score as f64).sum::<f64>() / games as f64;
    let best_score = reports.iter().map(|r| r.score).max().unwrap_or(0);
    let avg_max_tile = reports.iter().map(|r| r.max_tile as f64).sum::<f64>() / games as f64;
    let best_max_tile = reports.iter().map(|r| r.max_tile).max().unwrap_or(0);
    let count_ge = |t: u32| reports.iter().filter(|r| r.max_tile >= t).count();
    BenchSummary {
        games,
        total_moves,
        total_nodes,
        avg_score,
        best_score,
        avg_max_tile,
        best_max_tile,
        reached_2048: count_ge(2048),
        reached_4096: count_ge(4096),
        reached_8192: count_ge(8192),
        reached_16384: count_ge(16384),
        reached_goal: reports
            .iter()
            .filter(|r| r.reached_goal == Some(true))
            .count(),
        wall_secs,
    }
}

/// Run `cfg.games` full games spread over `cfg.threads` workers.
/// Returns per-game reports in completion order plus the aggregate summary.
pub fn run_bench(cfg: &BenchConfig) -> (Vec<GameReport>, BenchSummary) {
    let start = Instant::now();
    let (tx, rx) = mpsc::channel::<GameReport>();
    let next = AtomicUsize::new(0);
    let threads = cfg.threads.max(1);

    let reports: Vec<GameReport> = thread::scope(|scope| {
        for _ in 0..threads {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || {
                // Per-thread AI: own transposition table, no sharing.
                let mut ai = Expectimax::new(cfg.strength).unwrap_or_else(|e| panic!("{e}"));
                loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= cfg.games {
                        break;
                    }
                    // Fresh transposition table per game: keeps every game
                    // bit-for-bit reproducible regardless of which worker
                    // thread picks it up, and keeps node counters per-game.
                    ai.clear_tt();
                    let seed = cfg
                        .seed_base
                        .wrapping_add(idx as u64)
                        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    let mut game = Game::new(seed);
                    let mut report = game.play_ai(&mut ai, cfg.goal, cfg.max_moves, |_| {});
                    report.seed = seed;
                    if tx.send(report).is_err() {
                        break; // receiver dropped; stop early
                    }
                }
            });
        }
        drop(tx);

        let mut reports = Vec::with_capacity(cfg.games);
        for report in rx {
            if !cfg.quiet {
                println!(
                    "game {:>3}/{}  seed {:>20}  max tile {:>6}  score {:>8}  moves {:>6}  nodes {:>12}  {:>6.1}s",
                    reports.len() + 1,
                    cfg.games,
                    report.seed,
                    report.max_tile,
                    report.score,
                    report.moves,
                    report.nodes,
                    report.duration_secs
                );
            }
            reports.push(report);
        }
        reports
    });

    let wall = start.elapsed().as_secs_f64();
    (reports.clone(), summarize(&reports, wall))
}

/// Raw search throughput: repeatedly search a fixed, diverse set of positions
/// for `secs` seconds and report nodes/second and moves/second.
pub fn run_speed(secs: f64, strength: u8, threads: usize) -> SpeedResult {
    let positions: Vec<u64> = SPEED_POSITIONS
        .iter()
        .map(|v| crate::board::board_from_values(v).expect("valid test board"))
        .collect();
    let start = Instant::now();
    let totals: Vec<(u64, u64)> = thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..threads.max(1) {
            let positions = &positions;
            handles.push(scope.spawn(move || {
                let t0 = start; // Instant is Copy
                let mut ai = Expectimax::new(strength).unwrap_or_else(|e| panic!("{e}"));
                let mut nodes = 0u64;
                let mut moves = 0u64;
                while t0.elapsed().as_secs_f64() < secs {
                    for &b in positions.iter() {
                        let before = ai.nodes;
                        ai.best_move(b);
                        nodes += ai.nodes - before;
                        moves += 1;
                    }
                }
                (nodes, moves)
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().expect("worker"))
            .collect()
    });
    let wall = start.elapsed().as_secs_f64();
    let nodes: u64 = totals.iter().map(|t| t.0).sum();
    let moves: u64 = totals.iter().map(|t| t.1).sum();
    SpeedResult {
        secs: wall,
        threads,
        strength,
        nodes,
        moves,
    }
}

pub struct SpeedResult {
    pub secs: f64,
    pub threads: usize,
    pub strength: u8,
    pub nodes: u64,
    pub moves: u64,
}

impl SpeedResult {
    pub fn nodes_per_sec(&self) -> f64 {
        self.nodes as f64 / self.secs
    }
    pub fn moves_per_sec(&self) -> f64 {
        self.moves as f64 / self.secs
    }
    pub fn ms_per_move(&self) -> f64 {
        if self.moves == 0 {
            0.0
        } else {
            self.secs * 1000.0 / self.moves as f64
        }
    }
}

/// Curated boards spanning early / mid / late game (values, row-major).
static SPEED_POSITIONS: [[u32; 16]; 6] = [
    // opening
    [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
    // early
    [4, 2, 0, 0, 0, 4, 0, 0, 0, 0, 2, 0, 0, 0, 0, 2],
    // mid game
    [64, 32, 16, 8, 32, 16, 8, 4, 16, 8, 4, 2, 0, 4, 2, 0],
    // late game
    [
        1024, 512, 256, 128, 512, 256, 128, 64, 256, 128, 64, 32, 16, 8, 4, 0,
    ],
    // crowded
    [
        2048, 1024, 512, 256, 1024, 512, 256, 128, 512, 256, 128, 64, 32, 16, 8, 4,
    ],
    // endgame, nearly full
    [
        4096, 2048, 1024, 512, 2048, 1024, 512, 256, 1024, 512, 256, 128, 8, 4, 2, 0,
    ],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_two_quick_games() {
        let cfg = BenchConfig {
            games: 2,
            threads: 2,
            strength: 1,
            seed_base: 10,
            goal: Some(128),
            max_moves: 5000,
            quiet: true,
        };
        let (reports, summary) = run_bench(&cfg);
        assert_eq!(reports.len(), 2);
        assert_eq!(summary.games, 2);
        assert!(summary.wall_secs >= 0.0);
        // seeds must be distinct
        assert_ne!(reports[0].seed, reports[1].seed);
        // a game with the same seed and a fresh transposition table replays
        // identically (determinism across runs)
        let cfg2 = BenchConfig {
            games: 1,
            threads: 1,
            strength: 1,
            seed_base: 10,
            goal: Some(128),
            max_moves: 5000,
            quiet: true,
        };
        let (r2, _) = run_bench(&cfg2);
        let same = reports
            .iter()
            .find(|r| r.seed == r2[0].seed)
            .expect("seed present in first run");
        assert_eq!(same.moves, r2[0].moves);
        assert_eq!(same.score, r2[0].score);
    }

    #[test]
    fn speed_runs() {
        let r = run_speed(0.2, 3, 1);
        assert!(r.nodes > 0);
        assert!(r.moves > 0);
        assert!(r.nodes_per_sec() > 0.0);
    }
}
