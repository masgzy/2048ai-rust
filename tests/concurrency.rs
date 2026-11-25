//! Concurrency / race-condition regression tests.
//!
//! The benchmark design rule is: shared state is either immutable (tables,
//! positions) or handed out exactly once (game indices via `AtomicUsize`);
//! every mutable search object (Game, Expectimax + its transposition table)
//! is owned by exactly one thread. These tests hammer that design and also
//! verify per-thread determinism under contention.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

use engine2048::ai::Expectimax;
use engine2048::bench::{run_bench, BenchConfig};
use engine2048::game::{Game, GameReport};

/// Many concurrent quick games must all complete and produce valid reports.
#[test]
fn concurrent_games_all_valid() {
    let threads = 4usize;
    let games = 32usize;
    let (tx, rx) = mpsc::channel::<GameReport>();
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..threads {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || {
                let mut ai = Expectimax::new(1).unwrap();
                loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= games {
                        break;
                    }
                    let seed = 1000 + idx as u64;
                    let mut game = Game::new(seed);
                    let mut report = game.play_ai(&mut ai, Some(128), 3000, |_| {});
                    report.seed = seed;
                    tx.send(report).unwrap();
                }
            });
        }
        drop(tx);
        let mut count = 0usize;
        for r in rx {
            assert!(r.moves > 0, "a game made no moves");
            assert!(r.score > 0);
            assert!(r.max_tile >= 4);
            count += 1;
        }
        assert_eq!(count, games, "every game must complete exactly once");
    });
}

/// The same seed played under heavy contention must yield the identical game
/// as when played alone (no cross-thread state leakage).
///
/// Note: transposition-table entries do not record path probability (a
/// standard, documented approximation), so search results can depend on table
/// history — which is why every game starts from a cleared table, exactly like
/// the benchmark harness does.
#[test]
fn determinism_under_contention() {
    let alone = {
        let mut ai = Expectimax::new(2).unwrap();
        let mut game = Game::new(777);
        game.play_ai(&mut ai, Some(256), 5000, |_| {});
        (game.board, game.score, game.moves)
    };
    let (tx, rx) = mpsc::channel::<(u64, u32, u64)>();
    thread::scope(|scope| {
        for k in 0..4u64 {
            let tx = tx.clone();
            scope.spawn(move || {
                // One noise game per thread to build up table state first.
                let mut ai = Expectimax::new(2).unwrap();
                let mut noise = Game::new(900_000 + k);
                noise.play_ai(&mut ai, Some(64), 3000, |_| {});
                // Now the target game from a cleared table (bench semantics).
                ai.clear_tt();
                let mut game = Game::new(777);
                game.play_ai(&mut ai, Some(256), 5000, |_| {});
                tx.send((game.board, game.score, game.moves)).unwrap();
            });
        }
        drop(tx);
        for r in rx {
            assert_eq!(r, alone, "game differed when played concurrently");
        }
    });
}

/// The full bench harness (same code path as the CLI) with more games than
/// threads and vice versa — both work-claiming directions.
#[test]
fn bench_work_distribution() {
    for (games, threads) in [(7usize, 2usize), (2usize, 7usize), (1usize, 1usize)] {
        let cfg = BenchConfig {
            games,
            threads,
            strength: 1,
            seed_base: 5,
            goal: Some(64),
            max_moves: 4000,
            quiet: true,
        };
        let (reports, summary) = run_bench(&cfg);
        assert_eq!(reports.len(), games);
        assert_eq!(summary.games, games);
        let seeds: std::collections::HashSet<u64> = reports.iter().map(|r| r.seed).collect();
        assert_eq!(seeds.len(), games, "seeds must be unique");
    }
}
