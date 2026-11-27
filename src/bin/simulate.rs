//! simulate —— 无头自对弈基准：验证引擎强度、测量节点/秒。
//!
//! 运行 `simulate --help` 查看全部参数。min_ms = 0 表示全自动思考预算（与网页默认一致）。

use ai2048::board::{MoveTables, Rng};
use ai2048::engine::{self, TTPool};
use ai2048::eval::{Eval, Weights};

const USAGE: &str = "\
用法: simulate [选项]
  --games N        对局数（默认 10）
  --min-ms N       每步最少思考毫秒数，0 表示全自动预算（与网页默认一致）
  --max-depth N    搜索深度上限（默认 12，正常无需调整）
  --seed N         随机种子（默认 1）
  --ref-depth N    大于 0 时改用纯递归 expectimax 走固定 N 层（定位引擎差异用）
  --weights PATH   TD 学习权重文件，不存在则用内置权重（默认 weights.json）
  --version, -V    显示版本
  --help, -h       显示本帮助";

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
            "--max-depth" => max_depth = args.next().and_then(|v| v.parse().ok()).unwrap_or(12),
            "--seed" => seed = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--ref-depth" => ref_depth_arg = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--weights" => weights_path = args.next().unwrap_or_else(|| "weights.json".into()),
            "--version" | "-V" => {
                println!("simulate {}（ai2048）", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("{}", USAGE);
                return;
            }
            other => {
                eprintln!("未知参数 {}\n\n{}", other, USAGE);
                std::process::exit(2);
            }
        }
    }

    if games == 0 {
        eprintln!("--games 必须大于 0\n\n{}", USAGE);
        std::process::exit(2);
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
                if best.is_none_or(|(_, bv)| v > bv) {
                    best = Some((dir, v));
                }
            }
        }
        best.map(|(d, _)| d)
    }

    let (mut sum, mut best) = (0u64, 0u64);
    let (mut n2048, mut n4096, mut n8192) = (0u64, 0u64, 0u64);
    // 全部对局累计（此前误用"最后一局的值"做汇总统计）
    let (mut tot_nodes, mut tot_ms, mut tot_moves) = (0u64, 0u64, 0u64);
    let mut hist: [u64; 16] = [0; 16];

    for g in 1..=games {
        let mut b = 0u64;
        for _ in 0..2 {
            b = engine::spawn_tile(b, &mut rng);
        }
        let (mut gscore, mut gnodes, mut gms, mut gmoves) = (0u64, 0u64, 0u64, 0u64);
        loop {
            let picked = if ref_depth > 0 {
                ref_choose(&tables, &ev, b, ref_depth).map(|dir| engine::MoveResult {
                    dir,
                    score: 0.0,
                    depth: ref_depth,
                    nodes: 0,
                    ms: 0,
                    budget_ms: 0,
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
        let max_rank = (0..16)
            .map(|i| ai2048::board::nibble_at(b, i))
            .max()
            .unwrap_or(0);
        tot_nodes += gnodes;
        tot_ms += gms;
        tot_moves += gmoves;
        let o = engine::GameOutcome {
            score: gscore,
            max_rank,
            moves: gmoves,
            nodes: gnodes,
            ms: gms,
        };
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

    let nps = (tot_nodes * 1000).checked_div(tot_ms).unwrap_or(0);
    println!("================ 汇总 ================");
    println!(
        "平均分 {:.0}   最高分 {}   平均步数 {:.0}",
        sum as f64 / games as f64,
        best,
        tot_moves as f64 / games as f64
    );
    println!(
        "达成率: 2048={}/{}  4096={}/{}  8192={}/{}",
        n2048, games, n4096, games, n8192, games
    );
    println!(
        "吞吐:   平均 {} 节点/秒，平均每步 {} ms",
        nps,
        tot_ms.checked_div(tot_moves).unwrap_or(0)
    );
    print!("最高块分布:");
    for (rank, c) in hist.iter().enumerate() {
        if *c > 0 {
            print!("  {}x{}", 1 << rank, c);
        }
    }
    println!();
}
