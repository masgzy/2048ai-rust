//! AI strength regression tests.
//!
//! These games are fully seeded and deterministic (fresh transposition table
//! per game, no timers), so a failure always means the move quality actually
//! regressed — never flakes.

use engine2048::ai::Expectimax;
use engine2048::game::Game;

/// A strength-3 AI must comfortably clear the early game: reach 512 within
/// 400 moves on a fixed seed.
#[test]
fn strength3_reaches_512() {
    let mut ai = Expectimax::new(3).unwrap();
    let mut game = Game::new(42);
    let report = game.play_ai(&mut ai, Some(512), 400, |_| {});
    assert_eq!(
        report.reached_goal,
        Some(true),
        "seed 42 @ strength 3 failed to reach 512 (max tile {})",
        report.max_tile
    );
}

/// The headline regression gate: a strength-3 AI must reach the 2048 tile
/// (the game's namesake goal) on a fixed seed within a move budget.
#[test]
fn strength3_reaches_2048() {
    let mut ai = Expectimax::new(3).unwrap();
    let mut game = Game::new(7);
    let report = game.play_ai(&mut ai, Some(2048), 3000, |_| {});
    assert_eq!(
        report.reached_goal,
        Some(true),
        "seed 7 @ strength 3 failed to reach 2048 (max tile {})",
        report.max_tile
    );
}

/// A full low-strength game must end in a legal terminal state with a strong
/// final board (measured: seed 1234 @ strength 2 ends at 16384 / 271380
/// points; the thresholds below leave generous margin for refactors).
#[test]
fn full_game_is_strong_and_legal() {
    let mut ai = Expectimax::new(2).unwrap();
    let mut game = Game::new(1234);
    let report = game.play_ai(&mut ai, None, 20_000, |_| {});
    assert!(report.over, "game must terminate on its own");
    assert!(
        report.max_tile >= 4096,
        "max tile too low for a full strength-2 game: {}",
        report.max_tile
    );
    assert!(
        report.score >= 20_000,
        "score too low for a full game: {}",
        report.score
    );
    assert!(engine2048::board::is_dead(game.board));
}
