//! simulate —— 无头自对弈基准：验证引擎强度、测量节点/秒。
//!
//! 用法：
//!   simulate [--games 10] [--min-ms 0] [--max-depth 8] [--seed 1]
//! min_ms = 0 表示全自动思考预算（与网页默认一致）。

use ai2048::board::{MoveTables, Rng};
use ai2048::engine::{self, TTPool};
use ai2048::eval::{Eval, Weights};

fn main() {
    let mut games = 10u64;
    let mut min_ms = 0u64;
    let mut max_depth = 12u8;
    let mut seed = 1u64;
    let mut ref_depth_arg = 0u8;
    let mut weights_path = String::from("weights.json");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--games" => games = args.next().and_then(|v| v.parse().ok()).unwrap_or(10),
            "--min-ms" => min_ms = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--max-depth" => max_depth = args.next().and_then(|v| v.parse().ok()).unwrap_or(8),
            "--seed" => seed = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--ref-depth" => ref_depth_arg = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--weights" => weights_path = args.next().unwrap_or_else(|| "weights.json".into()),
            other => {
                eprintln!("未知参数 {}", other);
                return;
            }
        }
    }

    let tables = MoveTables::new();
    let ev = match ai2048::eval::load_file(&weights_path) {
        Ok(e) => {
            eprintln!("使用 TD 学习权重: {}", weights_path);
            e
        }
        Err(_) => Eval::build(&Weights::default()),
    };
    let pool = TTPool::new();
    let mut rng = Rng::new(seed);
    let ref_depth: u8 = ref_depth_arg;

    // 参考实现：纯递归 expectimax（无 TT / 无并行），用于二分定位
    fn ref_chance(t: &MoveTables, ev: &Eval, b: u64, depth: u8, cprob: f32) -> f32 {
        if depth == 0 || cprob < 1e-4 {
            return ev.eval(b);
        }
        let empties = ai2048::board::count_empty(b);
        if empties == 0 {
            return ev.eval(b);
        }
        let cp = cprob / empties as f32;
        let mut acc = 0.0f32;
        for idx in 0..16usize {
            if ai2048::board::nibble_at(b, idx) != 0 {
                continue;
            }
            for (rank, p) in [(1u16, 0.9f32), (2u16, 0.1f32)] {
                let nb = ai2048::board::set_nibble(b, idx, rank);
                acc += p * ref_player(t, ev, nb, depth, cp * p);
            }
        }
        acc / empties as f32
    }

    fn ref_player(t: &MoveTables, ev: &Eval, b: u64, depth: u8, cprob: f32) -> f32 {
        let mut best = 0.0f32;
        let mut any = false;
        for dir in 0..4usize {
            let (nb, _, m) = t.move_board(b, dir);
            if m {
                any = true;
                let v = ref_chance(t, ev, nb, depth - 1, cprob);
                if v > best {
                    best = v;
                }
            }
        }
        if !any {
            0.0
        } else {
            best
        }
    }

    fn ref_choose(t: &MoveTables, ev: &Eval, b: u64, depth: u8) -> Option<u8> {
        let mut best: Option<(u8, f32)> = None;
        for dir in 0..4u8 {
            let (nb, _, m) = t.move_board(b, dir as usize);
            if m {
                let v = ref_chance(t, ev, nb, depth.saturating_sub(1), 1.0);
                if best.map_or(true, |(_, bv)| v > bv) {
                    best = Some((dir, v));
                }
            }
        }
        best.map(|(d, _)| d)
    }

    let (mut sum, mut best) = (0u64, 0u64);
    let (mut n2048, mut n4096, mut n8192) = (0u64, 0u64, 0u64);
    let (mut nodes, mut ms, mut moves) = (0u64, 0u64, 0u64);
    let mut hist: [u64; 16] = [0; 16];

    for g in 1..=games {
        let mut b = 0u64;
        for _ in 0..2 {
            b = engine::spawn_tile(b, &mut rng);
        }
        let (mut score, mut gscore) = (0u64, 0u64);
        let (mut gnodes, mut gms, mut gmoves) = (0u64, 0u64, 0u64);
        loop {
            let picked = if ref_depth > 0 {
                ref_choose(&tables, &ev, b, ref_depth).map(|dir| {
                    engine::MoveResult { dir, score: 0.0, depth: ref_depth, nodes: 0, ms: 0, budget_ms: 0 }
                })
            } else {
                engine::choose_move(&pool, &tables, &ev, b, min_ms, max_depth).ok()
            };
            match picked {
                Some(r) => {
                    if g == 1 && (gmoves < 25 || gmoves % 200 == 0) {
                        eprintln!(
                            "  move {:>4}: dir={} depth={} nodes={:>9} ms={:>4} score={}",
                            gmoves, r.dir, r.depth, r.nodes, r.ms, gscore
                        );
                    }
                    let (nb, gained, _) = tables.move_board(b, r.dir as usize);
                    gscore += gained as u64;
                    gnodes += r.nodes;
                    gms += r.ms;
                    gmoves += 1;
                    b = engine::spawn_tile(nb, &mut rng);
                }
                None => break,
            }
        }
        let max_rank = (0..16).map(|i| ai2048::board::nibble_at(b, i)).max().unwrap_or(0);
        score = gscore;
        moves = gmoves;
        nodes = gnodes;
        ms = gms;
        let o = engine::GameOutcome { score, max_rank, moves, nodes, ms };
        sum += o.score;
        best = best.max(o.score);
        hist[o.max_rank as usize] += 1;
        if o.max_rank >= 11 {
            n2048 += 1;
        }
        if o.max_rank >= 12 {
            n4096 += 1;
        }
        if o.max_rank >= 13 {
            n8192 += 1;
        }
        eprintln!(
            "局 {:>4}: 分数 {:>7}  最高块 {:>5}  步数 {:>4}",
            g,
            o.score,
            1 << o.max_rank,
            o.moves
        );
    }

    let nps = if ms > 0 { nodes * 1000 / ms.max(1) } else { 0 };
    println!("================ 汇总 ================");
    println!(
        "平均分 {:.0}   最高分 {}   平均步数 {:.0}",
        sum as f64 / games as f64,
        best,
        moves as f64 / games as f64
    );
    println!("达成率: 2048={}/{}  4096={}/{}  8192={}/{}", n2048, games, n4096, games, n8192, games);
    println!(
        "吞吐:   平均 {} 节点/秒，平均每步 {} ms",
        nps,
        if moves > 0 { ms / moves } else { 0 }
    );
    print!("最高块分布:");
    for (rank, c) in hist.iter().enumerate() {
        if *c > 0 {
            print!("  {}x{}", 1 << rank, c);
        }
    }
    println!();
}
