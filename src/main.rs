//! 2048ai — high-performance 2048 AI.
//!
//! Subcommands:
//!   play      watch the AI play one game (visualised)
//!   bench     run N automated games in parallel and report statistics
//!   speed     measure raw search throughput (nodes/second)
//!   bestmove  read a board from stdin, print the best move (integration API)

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use engine2048::ai::Expectimax;
use engine2048::bench::{self, BenchConfig, SpeedResult};
use engine2048::cli::{self, Options};
use engine2048::game::Game;
use engine2048::render;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            if e == "__help__" {
                print_help(&args[0]);
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {e}");
            eprintln!("run '2048ai help' for usage");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    match args[0].as_str() {
        "play" => cmd_play(&args[1..]),
        "bench" => cmd_bench(&args[1..]),
        "speed" => cmd_speed(&args[1..]),
        "bestmove" => cmd_bestmove(&args[1..]),
        "help" | "--help" | "-h" => {
            print_help("");
            Ok(ExitCode::SUCCESS)
        }
        "version" | "--version" | "-V" => {
            println!("2048ai {VERSION}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

fn cmd_play(args: &[String]) -> Result<ExitCode, String> {
    let opts = Options::parse(
        args,
        &["seed", "strength", "goal", "render-every", "max-moves"],
    )?;
    let seed: u64 = opts.num("seed", 0)?;
    let strength: u8 = opts.num("strength", 4)?;
    let goal: Option<u32> = match opts.get("goal") {
        Some(s) => Some(parse_tile(s)?),
        None => None,
    };
    let render_every: u64 = opts.num("render-every", 1)?.max(1);
    let max_moves: u64 = opts.num("max-moves", 200_000)?;
    let quiet = opts.has("quiet");
    let json = opts.has("json");
    let color = match opts.get("color") {
        Some("always") => true,
        Some("never") => false,
        _ => std::io::stdout().is_terminal(),
    };
    let clear = opts.has("clear") || (color && !opts.has("no-clear"));

    // A zero seed means "pick one from the clock" (still printed for reference).
    let seed = if seed == 0 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1
    } else {
        seed
    };

    let mut game = Game::new(seed);
    let mut ai = Expectimax::new(strength)?;
    let tty = std::io::stdout().is_terminal();

    let report = game.play_ai(&mut ai, goal, max_moves, |g| {
        if quiet || json {
            return;
        }
        if g.moves % render_every != 0 {
            return;
        }
        if clear && tty {
            // Move cursor home and clear below instead of full clear to reduce
            // flicker.
            print!("\x1b[H\x1b[J");
        }
        print!("{}", render::render_board(g.board, color));
        println!("{}", render::status_line(g.score, g.moves, g.max_tile));
        println!("move #{} — thinking...", g.moves);
    });

    let final_line = if report.over {
        "GAME OVER"
    } else if report.reached_goal == Some(true) {
        "GOAL REACHED"
    } else {
        "STOPPED"
    };

    if json {
        println!(
            "{{\"seed\": {}, \"score\": {}, \"max_tile\": {}, \"moves\": {}, \"nodes\": {}, \"duration_secs\": {:.3}, \"over\": {}, \"reached_goal\": {}, \"status\": \"{}\"}}",
            report.seed,
            report.score,
            report.max_tile,
            report.moves,
            report.nodes,
            report.duration_secs,
            report.over,
            report.reached_goal.unwrap_or(false),
            final_line
        );
    } else {
        print!("{}", render::render_board(game.board, color));
        println!(
            "{}",
            render::status_line(game.score, game.moves, game.max_tile)
        );
        println!(
            "{final_line} — score {} | max tile {} | {} moves | {} nodes | {:.1}s | seed {}",
            report.score,
            report.max_tile,
            report.moves,
            report.nodes,
            report.duration_secs,
            report.seed
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_bench(args: &[String]) -> Result<ExitCode, String> {
    let opts = Options::parse(
        args,
        &["games", "threads", "strength", "seed", "goal", "max-moves"],
    )?;
    let games: usize = opts.num("games", 8)?.max(1);
    let threads: usize = opts.num("threads", default_threads())?.max(1);
    let strength: u8 = opts.num("strength", 4)?;
    let seed: u64 = opts.num("seed", 1)?;
    let goal = match opts.get("goal") {
        Some(s) => Some(parse_tile(s)?),
        None => None,
    };
    let max_moves: u64 = opts.num("max-moves", 200_000)?;
    let json = opts.has("json");
    let cfg = BenchConfig {
        games,
        threads,
        strength,
        seed_base: seed,
        goal,
        max_moves,
        quiet: json,
    };

    if !json {
        println!(
            "2048ai bench: {games} games | {threads} threads | strength {strength} | goal {}",
            goal.map(|g| g.to_string()).unwrap_or_else(|| "none".into())
        );
    }
    let (_, summary) = bench::run_bench(&cfg);

    if json {
        println!(
            "{{\"games\": {}, \"strength\": {}, \"threads\": {}, \"goal\": {}, \"total_moves\": {}, \"total_nodes\": {}, \"avg_score\": {:.1}, \"best_score\": {}, \"avg_max_tile\": {:.1}, \"best_max_tile\": {}, \"reached_2048\": {}, \"reached_4096\": {}, \"reached_8192\": {}, \"reached_16384\": {}, \"reached_goal\": {}, \"wall_secs\": {:.3}, \"moves_per_sec\": {:.1}, \"nodes_per_sec\": {:.1}, \"games_per_min\": {:.2}}}",
            summary.games,
            strength,
            threads,
            goal.unwrap_or(0),
            summary.total_moves,
            summary.total_nodes,
            summary.avg_score,
            summary.best_score,
            summary.avg_max_tile,
            summary.best_max_tile,
            summary.reached_2048,
            summary.reached_4096,
            summary.reached_8192,
            summary.reached_16384,
            summary.reached_goal,
            summary.wall_secs,
            summary.moves_per_sec(),
            summary.nodes_per_sec(),
            summary.games_per_min()
        );
    } else {
        println!("---------------- benchmark summary ----------------");
        println!("games                : {}", summary.games);
        println!("threads              : {threads}");
        println!("strength             : {strength}");
        println!("total moves          : {}", summary.total_moves);
        println!("total search nodes   : {}", summary.total_nodes);
        println!(
            "avg / best score     : {:.0} / {}",
            summary.avg_score, summary.best_score
        );
        println!(
            "avg / best max tile  : {:.0} / {}",
            summary.avg_max_tile, summary.best_max_tile
        );
        println!(
            "reached 2048         : {}/{}",
            summary.reached_2048, summary.games
        );
        println!(
            "reached 4096         : {}/{}",
            summary.reached_4096, summary.games
        );
        println!(
            "reached 8192         : {}/{}",
            summary.reached_8192, summary.games
        );
        println!(
            "reached 16384        : {}/{}",
            summary.reached_16384, summary.games
        );
        if let Some(g) = goal {
            println!(
                "reached goal {g:<8}: {}/{}",
                summary.reached_goal, summary.games
            );
        }
        println!("wall time            : {:.1}s", summary.wall_secs);
        println!(
            "throughput           : {:.0} moves/s | {:.0} nodes/s | {:.2} games/min",
            summary.moves_per_sec(),
            summary.nodes_per_sec(),
            summary.games_per_min()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_speed(args: &[String]) -> Result<ExitCode, String> {
    let opts = Options::parse(args, &["secs", "strength", "threads"])?;
    let secs: f64 = opts.num("secs", 5.0)?;
    if !(0.1..=3600.0).contains(&secs) {
        return Err(format!("--secs must be in 0.1..=3600, got {secs}"));
    }
    let strength: u8 = opts.num("strength", 5)?;
    let threads: usize = opts.num("threads", default_threads())?.max(1);
    let json = opts.has("json");

    let r: SpeedResult = bench::run_speed(secs, strength, threads);
    if json {
        println!(
            "{{\"secs\": {:.3}, \"strength\": {}, \"threads\": {}, \"nodes\": {}, \"moves\": {}, \"nodes_per_sec\": {:.1}, \"moves_per_sec\": {:.1}, \"ms_per_move\": {:.3}}}",
            r.secs,
            r.strength,
            r.threads,
            r.nodes,
            r.moves,
            r.nodes_per_sec(),
            r.moves_per_sec(),
            r.ms_per_move()
        );
    } else {
        println!(
            "2048ai speed: strength {} | {} threads | {:.1}s",
            r.strength, r.threads, r.secs
        );
        println!("search nodes         : {}", r.nodes);
        println!("best-move calls      : {}", r.moves);
        println!("nodes per second     : {:.0}", r.nodes_per_sec());
        println!("moves per second     : {:.1}", r.moves_per_sec());
        println!("avg time per move    : {:.3} ms", r.ms_per_move());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_bestmove(args: &[String]) -> Result<ExitCode, String> {
    let opts = Options::parse(args, &["strength"])?;
    let strength: u8 = opts.num("strength", 4)?;
    let json = opts.has("json");

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    let b = cli::parse_board_input(&input)?;

    let mut ai = Expectimax::new(strength)?;
    match ai.best_move(b) {
        None => {
            if json {
                println!("{{\"best\": null, \"game_over\": true}}");
            } else {
                println!("GAME OVER (no legal move)");
            }
            Ok(ExitCode::SUCCESS)
        }
        Some((m, v)) => {
            if json {
                println!(
                    "{{\"best\": \"{}\", \"value\": {:.3}, \"depth\": {}, \"nodes\": {}}}",
                    m.name(),
                    v,
                    ai.last_depth,
                    ai.nodes
                );
            } else {
                println!(
                    "best: {} (value {v:.3}, depth {}, {} nodes)",
                    m.name(),
                    ai.last_depth,
                    ai.nodes
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn parse_tile(s: &str) -> Result<u32, String> {
    let v: u32 = s.parse().map_err(|_| format!("invalid tile value '{s}'"))?;
    if v == 0 || !v.is_power_of_two() {
        return Err(format!("tile value must be a power of two, got {v}"));
    }
    Ok(v)
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn print_usage() {
    println!("2048ai {VERSION} — high-performance 2048 AI (expectimax + bitboard)");
    println!();
    println!("USAGE:");
    println!("  2048ai <COMMAND> [OPTIONS]");
    println!();
    println!("COMMANDS:");
    println!("  play       watch the AI play one game (default visualisation)");
    println!("  bench      run N automated games in parallel, report statistics");
    println!("  speed      measure raw search throughput (nodes/second)");
    println!("  bestmove   read a board from stdin, print the best move");
    println!("  help       detailed help");
    println!("  version    print version");
    println!();
    println!("Run '2048ai help' for full option documentation.");
}

fn print_help(cmd: &str) {
    let _ = cmd;
    print_usage();
    println!("OPTIONS (common):");
    println!("  --strength 1..=7    search depth preset (1 fast .. 7 deep, default 4)");
    println!("  --seed N            RNG seed (0 = clock-based)");
    println!("  --json              machine-readable output");
    println!();
    println!("play:");
    println!("  --goal TILE         stop once TILE (e.g. 4096) is reached");
    println!("  --render-every N    render every Nth move (default 1)");
    println!("  --max-moves N       safety cap (default 200000)");
    println!("  --quiet             no board rendering");
    println!("  --color always|never|auto");
    println!("  --no-clear          do not clear the screen between frames");
    println!("  --clear             force screen clearing even when piping");
    println!();
    println!("bench:");
    println!("  --games N           number of games (default 8)");
    println!("  --threads N         worker threads (default: CPU count)");
    println!("  --goal TILE         count games that reach TILE");
    println!();
    println!("speed:");
    println!("  --secs F            measurement duration (default 5)");
    println!("  --threads N         worker threads");
    println!();
    println!("bestmove:");
    println!("  reads 16 whitespace-separated values (row-major, 0=empty) or a hex");
    println!("  u64 of packed ranks from stdin; prints UP/DOWN/LEFT/RIGHT");
    println!();
    println!("EXIT CODES: 0 success, 2 usage/input error");
}
