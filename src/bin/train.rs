//! train —— TD 强化学习自博弈训练器（OTD / OTC / OTD+TC，多线程）。
//!
//! 方法族与 TDL2048+（moporgic/TDL2048，Guei et al. IEEE TG 2021）同源：
//! - 乐观初始化（OI）：`--vinit`（默认 80000，论文 8x6 网络最优区间 80k~320k
//!   折中），均摊到 8 个元组权重；
//! - TC 学习：β = |E|/A 逐权重调制学习率；
//! - OTD+TC：`--ptc 0.1` 表示最后 10% 局数切到 TC 微调（论文配方）；
//! - 多阶段：`--stages 8192`（默认，两阶段）或 `--stages 8192,16384`（三阶段）；
//! - 多线程：`--threads N`，每线程独立网络自博弈（lock-free，无共享竞争），
//!   每 `--merge-every N` 局把各线程权重平均合并一次并重新分发。
//!   这是 TDL2048 20 线程 lock-free optimistic 并行的保守变体：
//!   论文做法是全部线程直接写同一网络（依赖 α 很小的低冲突假设），
//!   零依赖实现下用"独立训练 + 周期平均"达到同样的无锁效果且可重现。
//!
//! 输出着色：Catppuccin（Mocha/Latte），`--theme` / `--color` / NO_COLOR 等降级链见 `color` 模块。
//!
//! 运行 `train --help` 查看全部参数。

use std::sync::mpsc;

use ai2048::board::{MoveTables, Rng};
use ai2048::color::{self, ColorMode, Theme};
use ai2048::learn::{self, Mode, Stages};

const USAGE: &str = "\
用法: train [选项]
  --games N        训练局数（默认 1000）
  --threads N      并行训练线程数（默认 1；>1 时各线程独立网络，定期平均）
  --merge-every N  多线程模式下每 N 局合并一次权重（默认 250）
  --mode M         学习器: otd | otc | otd-tc（默认 otd-tc，即 OTD+TC）
  --vinit X        乐观初始化值 Vinit，0 关闭（默认 80000）
  --ptc X          OTD+TC 中 TC 微调阶段占比 0~1（默认 0.1）
  --alpha X        学习率（OTD 默认 0.1；OTC 建议 1.0，由 β 调制）
  --eps X          ε-greedy 探索概率（默认 0.01；OI 已提供隐式探索）
  --stages L       多阶段阈值（rank）：如 8192 或 8192,16384；空串=单阶段
                   （默认 8192，即两阶段；输入按块值换算 rank）
  --out PATH       权重输出文件（默认 weights.json）
  --resume PATH    从已有权重文件继续训练（解析失败则从头开始）
  --seed N         随机种子（默认 1）
  --theme T        配色口味: mocha | latte（默认 mocha，亦受 CLI_THEME 影响）
  --color C        颜色开关: auto | always | never（降级链见 README）
  --version, -V    显示版本
  --help, -h       显示本帮助";

/// 块值 → rank（log2）。8192 → 13。
fn value_to_rank(v: u64) -> Option<u16> {
    if v.count_ones() != 1 || v < 4 {
        return None;
    }
    Some(v.trailing_zeros() as u16)
}

fn parse_stages(spec: &str) -> Result<Vec<u16>, String> {
    if spec.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        let v: u64 = part
            .trim()
            .parse()
            .map_err(|_| format!("--stages 的值非法: {}", part))?;
        let r =
            value_to_rank(v).ok_or_else(|| format!("--stages 的值必须是 >=4 的 2 的幂: {}", v))?;
        if !(4..=15).contains(&r) {
            return Err(format!("--stages 阈值须在 16..32768 之间: {}", v));
        }
        out.push(r);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

struct Opts {
    games: u64,
    threads: usize,
    merge_every: u64,
    mode: Mode,
    vinit: f32,
    ptc: f32,
    alpha: f32,
    eps: f32,
    stages: Vec<u16>,
    out: String,
    resume: Option<String>,
    seed: u64,
}

fn save(path: &str, nets: &Stages, games: u64) {
    // 多阶段：{"magic":"ai2048-td","games":N,"stages":[13],"tables":[[...],[...]]}
    // 单阶段：兼容旧格式 {"magic":"ai2048-td","games":N,"table":[...]}
    let (n_stages, tables) = nets.to_json_parts();
    let mut s = String::with_capacity(65536 * 8 * (n_stages + 1));
    s.push_str("{\"magic\":\"ai2048-td\",\"games\":");
    s.push_str(&games.to_string());
    if n_stages > 0 {
        s.push_str(",\"stages\":[");
        for (i, &t) in nets.thresholds.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&(1u64 << t).to_string());
        }
        s.push_str("],\"tables\":[");
        for (i, t) in tables.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(t);
        }
        s.push_str("]}");
    } else {
        s.push_str(",\"table\":");
        s.push_str(&tables[0]);
        s.push('}');
    }
    match std::fs::write(path, s) {
        Ok(()) => color::println_ok(&format!("已保存 {}（{} 局）", path, games)),
        Err(e) => color::println_err(&format!("保存 {} 失败: {}", path, e)),
    }
}

fn load_resume(path: &str) -> Result<Stages, String> {
    let ev = ai2048::eval::load_stages(path)?;
    Ok(ev)
}

/// 单线程训练段：`games` 局写进 `nets`，每局回调进度。
fn train_chunk(
    nets: &mut Stages,
    t: &MoveTables,
    games: u64,
    opts: &Opts,
    seed: u64,
    start_game: u64,
    on_game: &mut dyn FnMut(u64, u64, u16),
) {
    let mut rng = Rng::new(seed);
    let tc_start = ((opts.games as f32 * (1.0 - opts.ptc)) as u64).max(1);
    for i in 0..games {
        let g = start_game + i + 1;
        // OTD+TC：前 (1-ptc) 用 TD，之后切 TC（按全局局号）
        let mode = match opts.mode {
            Mode::OtdTc if g >= tc_start => Mode::Otc,
            m => m,
        };
        let (score, max_rank, _moves) =
            learn::play_and_learn(nets, t, &mut rng, mode, opts.alpha, opts.eps);
        on_game(g, score, max_rank);
    }
}

fn main() {
    let mut o = Opts {
        games: 1000,
        threads: 1,
        merge_every: 250,
        mode: Mode::OtdTc,
        vinit: 80_000.0,
        ptc: 0.1,
        alpha: 0.1,
        eps: 0.01,
        stages: vec![13],
        out: String::from("weights.json"),
        resume: None,
        seed: 1,
    };
    let mut cli_color = ColorMode::Auto;
    let mut cli_theme: Option<Theme> = None;
    let mut stages_spec = String::from("8192");

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("参数 {} 缺少取值", a));
        match a.as_str() {
            "--games" => o.games = parse_or_exit(&value(), 1000),
            "--threads" => o.threads = parse_or_exit(&value(), 1),
            "--merge-every" => o.merge_every = parse_or_exit(&value(), 250),
            "--mode" => {
                o.mode = match value().as_deref() {
                    Ok("otd") => Mode::Otd,
                    Ok("otc") => Mode::Otc,
                    Ok("otd-tc") | Ok("otdtc") => Mode::OtdTc,
                    Ok(x) => {
                        eprintln!("--mode 须为 otd | otc | otd-tc，当前 {}", x);
                        std::process::exit(2);
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        std::process::exit(2);
                    }
                }
            }
            "--vinit" => o.vinit = parse_or_exit(&value(), 80_000.0),
            "--ptc" => {
                o.ptc = parse_or_exit(&value(), 0.1);
                if !(0.0..=1.0).contains(&o.ptc) {
                    eprintln!("--ptc 须在 0~1 之间");
                    std::process::exit(2);
                }
            }
            "--alpha" => o.alpha = parse_or_exit(&value(), 0.1),
            "--eps" => o.eps = parse_or_exit(&value(), 0.01),
            "--stages" => stages_spec = value().unwrap_or_default(),
            "--out" => o.out = value().unwrap_or_default(),
            "--resume" => o.resume = value().ok(),
            "--seed" => o.seed = parse_or_exit(&value(), 1),
            "--theme" => {
                cli_theme = Some(match value().as_deref() {
                    Ok("mocha") | Ok("dark") => Theme::Mocha,
                    Ok("latte") | Ok("light") => Theme::Latte,
                    Ok(x) => {
                        eprintln!("--theme 须为 mocha | latte，当前 {}", x);
                        std::process::exit(2);
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        std::process::exit(2);
                    }
                })
            }
            "--color" => {
                cli_color = match value().as_deref() {
                    Ok("auto") => ColorMode::Auto,
                    Ok("always") => ColorMode::On,
                    Ok("never") => ColorMode::Off,
                    Ok(x) => {
                        eprintln!("--color 须为 auto | always | never，当前 {}", x);
                        std::process::exit(2);
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        std::process::exit(2);
                    }
                }
            }
            "--version" | "-V" => {
                println!("train {}（ai2048）", env!("CARGO_PKG_VERSION"));
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
    if o.games == 0 {
        eprintln!("--games 必须大于 0\n\n{}", USAGE);
        std::process::exit(2);
    }
    if o.threads == 0 {
        eprintln!("--threads 必须大于 0\n\n{}", USAGE);
        std::process::exit(2);
    }
    o.stages = match parse_stages(&stages_spec) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}\n\n{}", e, USAGE);
            std::process::exit(2);
        }
    };
    // OTC 模式默认 α=1.0（论文：TC 需要更大初始学习率，由 β 调制）
    if o.mode == Mode::Otc && o.alpha == 0.1 {
        o.alpha = 1.0;
    }

    color::init(cli_color, cli_theme);

    let t = MoveTables::new();
    let mut nets = match &o.resume {
        Some(p) => match load_resume(p) {
            Ok(x) => {
                color::println_info(&format!("从 {} 恢复训练（{} 阶段）", p, x.stage_count()));
                x
            }
            Err(e) => {
                color::println_warn(&format!("恢复失败: {}（从头开始）", e));
                Stages::new(o.stages.clone(), o.vinit)
            }
        },
        None => Stages::new(o.stages.clone(), o.vinit),
    };

    // 训练参数摘要
    let mode_name = match o.mode {
        Mode::Otd => "OTD",
        Mode::Otc => "OTC",
        Mode::OtdTc => "OTD+TC",
    };
    println!(
        "{}",
        color::title(&format!("ai2048 TD 训练器 {}", mode_name))
    );
    println!(
        "  {}",
        color::muted(&format!(
            "局数 {} 线程 {} α {} eps {} Vinit {} Ptc {} 阶段 {:?} 种子 {}",
            o.games, o.threads, o.alpha, o.eps, o.vinit, o.ptc, o.stages, o.seed
        ))
    );

    let start = std::time::Instant::now();
    if o.threads <= 1 {
        let (mut best, mut recent): (u64, Vec<u64>) = (0, Vec::new());
        let (mut n2048, mut n8192) = (0u64, 0u64);
        let mut on_game = |g: u64, score: u64, max_rank: u16| {
            best = best.max(score);
            recent.push(score);
            if recent.len() > 100 {
                recent.remove(0);
            }
            if max_rank >= 11 {
                n2048 += 1;
            }
            if max_rank >= 13 {
                n8192 += 1;
            }
            if g % 25 == 0 {
                let avg = recent.iter().sum::<u64>() as f64 / recent.len() as f64;
                println!(
                    "  {}",
                    color::muted(&format!(
                        "局 {:>6}  近100局平均 {}  本局 {} (最高块 {})  最佳 {}  2048+ {} / 8192+ {}",
                        g,
                        avg as u64,
                        color::number(&score.to_string()),
                        1u64 << max_rank,
                        color::number(&best.to_string()),
                        color::number(&n2048.to_string()),
                        color::number(&n8192.to_string())
                    ))
                );
            }
        };
        train_chunk(&mut nets, &t, o.games, &o, o.seed, 0, &mut on_game);
    } else {
        // 多线程：每线程独立网络 + 周期平均。
        // 划分：把总局数切成 ceil(games / merge_every / threads) 轮，
        // 每轮各线程跑 merge_every 局，然后把所有线程网络与主网络平均。
        let per_merge = o.merge_every.max(1);
        let mut game_done = 0u64;
        let main_seed = o.seed;
        let threads = o.threads;
        let alpha = o.alpha;
        let eps = o.eps;
        let vinit = o.vinit;
        let thresholds = o.stages.clone();

        while game_done < o.games {
            let chunk = per_merge.min(o.games - game_done);
            let (tx, rx) = mpsc::channel::<Stages>();
            let chunks: Vec<u64> = split_evenly(chunk, threads);
            std::thread::scope(|s| {
                for (ti, &c) in chunks.iter().enumerate() {
                    if c == 0 {
                        continue;
                    }
                    let tx = tx.clone();
                    let t = &t;
                    let thresholds = &thresholds;
                    let base_game = game_done;
                    s.spawn(move || {
                        let mut local = Stages::new(thresholds.clone(), vinit);
                        let mut rng = Rng::new(main_seed.wrapping_add(1_000_003 * (ti as u64 + 1)));
                        let tc_start = ((o.games as f32 * (1.0 - o.ptc)) as u64).max(1);
                        for i in 0..c {
                            let g = base_game + i + 1;
                            let mode = match o.mode {
                                Mode::OtdTc if g >= tc_start => Mode::Otc,
                                m => m,
                            };
                            let _ =
                                learn::play_and_learn(&mut local, t, &mut rng, mode, alpha, eps);
                        }
                        let _ = tx.send(local);
                    });
                }
                drop(tx);
                // 主线程自己也训练一份（不闲着）
            });
            // 收集：主网络 = (主网络 + Σ 各线程网络) / (n+1)
            let mut received = 0usize;
            for local in rx {
                nets.merge_from(&local, 1.0);
                received += 1;
            }
            if received > 0 {
                nets.scale(1.0 / (received + 1) as f32);
            }
            game_done += chunk;
            println!(
                "  {}",
                color::info(&format!(
                    "已训练 {} / {} 局（{} 线程，本轮合并 {} 份网络）",
                    color::number(&game_done.to_string()),
                    color::number(&o.games.to_string()),
                    threads,
                    received
                ))
            );
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let gps = o.games as f64 / elapsed;
    color::println_ok(&format!(
        "训练完成: {} 局 / {:.1}s = {} 局/秒",
        o.games, elapsed, gps as u64
    ));
    save(&o.out, &nets, o.games);
}

fn split_evenly(total: u64, parts: usize) -> Vec<u64> {
    let parts = parts.max(1);
    let base = total / parts as u64;
    let rem = total % parts as u64;
    (0..parts)
        .map(|i| base + if (i as u64) < rem { 1 } else { 0 })
        .collect()
}

fn parse_or_exit<T: std::str::FromStr>(raw: &Result<String, String>, default: T) -> T {
    match raw {
        Ok(v) => v.parse().unwrap_or(default),
        Err(e) => {
            eprintln!("{}\n\n{}", e, USAGE);
            std::process::exit(2);
        }
    }
}
