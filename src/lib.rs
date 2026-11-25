//! engine2048 — library core of the 2048 AI.
//!
//! Modules:
//! - [`board`]: 64-bit bitboard engine and precomputed move/score/heuristic tables
//! - [`ai`]: expectimax search with transposition table and probability pruning
//! - [`game`]: game state, seeded PCG32 RNG, automated play loop
//! - [`bench`]: parallel benchmark and throughput measurement harness
//! - [`cli`]: dependency-free argument / board-input parsing
//! - [`render`]: terminal visualisation

pub mod ai;
pub mod bench;
pub mod board;
pub mod cli;
pub mod game;
pub mod render;
