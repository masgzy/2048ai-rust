//! train —— TD 强化学习自博弈训练器（TD(0) on afterstates，线性元组价值函数）。
//!
//! 价值函数与引擎评估同构：V(board) = Σ 4 行 + Σ 4 列 查同一张 65536 项表。
//! 学习流程（afterstate TD，经典 2048 TD 方案）：
//!   1. 在当前盘面 s 上 ε-greedy 选着法 a，得到 afterstate a_s、得分 r、估值 q；
//!   2. 随机生成块得到 s'；若 s' 已死，target = r；
//!      否则 target = r + max_a' V(move(s', a'))；
//!   3. δ = target − q；对 a_s 的 8 个元组（4 行 + 4 列）：w[i] += α·δ；
//!   4. s ← s'，循环直到死局。
//!
//! 产出 weights.json，ai2048 服务器启动时自动加载。
//!
//! 运行 `train --help` 查看全部参数。

use ai2048::board::{self, MoveTables, Rng};
use ai2048::engine;

const USAGE: &str = "\
用法: train [选项]
  --games N        训练局数（默认 1000）
  --alpha X        学习率（默认 0.001）
  --eps X          ε-greedy 探索概率（默认 0.05）
  --out PATH       权重输出文件（默认 weights.json）
  --resume PATH    从已有权重文件继续训练（解析失败则从头开始）
  --seed N         随机种子（默认 1）
  --version, -V    显示版本
  --help, -h       显示本帮助";

struct Value {
    w: Vec<f32>,
}

impl Value {
    fn new() -> Self {
        Value {
            w: vec![0f32; 65536],
        }
    }

    fn from_json(path: &str) -> Result<Self, String> {
        let ev = ai2048::eval::load_file(path)?;
        Ok(Value { w: ev.heur })
    }

    /// V(board) = 4 行 + 4 列 查表求和
    #[inline]
    fn v(&self, b: u64) -> f32 {
        let t = board::transpose(b);
        let mut s = 0.0f32;
        s += self.w[(b & 0xFFFF) as usize]
            + self.w[((b >> 16) & 0xFFFF) as usize]
            + self.w[((b >> 32) & 0xFFFF) as usize]
            + self.w[((b >> 48) & 0xFFFF) as usize];
        s += self.w[(t & 0xFFFF) as usize]
            + self.w[((t >> 16) & 0xFFFF) as usize]
            + self.w[((t >> 32) & 0xFFFF) as usize]
            + self.w[((t >> 48) & 0xFFFF) as usize];
        s
    }

    /// 对 afterstate 的 8 个元组做 TD 更新
    #[inline]
    fn update(&mut self, b: u64, delta: f32, alpha: f32) {
        let t = board::transpose(b);
        let upd = |w: &mut Vec<f32>, base: u64| {
            for i in 0..4usize {
                let idx = ((base >> (16 * i)) & 0xFFFF) as usize;
                w[idx] += alpha * delta;
            }
        };
        upd(&mut self.w, b);
        upd(&mut self.w, t);
    }
}

/// (afterstate, gained, dir, value)
type Choice = (u64, u32, u8, f32);

/// 枚举所有可行着法
fn options(v: &Value, t: &MoveTables, b: u64) -> Vec<Choice> {
    let mut out = Vec::with_capacity(4);
    for dir in 0..4u8 {
        let (nb, gained, moved) = t.move_board(b, dir as usize);
        if moved {
            out.push((nb, gained, dir, v.v(nb)));
        }
    }
    out
}

fn max_tile(b: u64) -> u16 {
    let mut m = 0u16;
    for i in 0..16 {
        m = m.max(board::nibble_at(b, i));
    }
    m
}

fn save(path: &str, v: &Value, games: u64) {
    let mut s = String::with_capacity(65536 * 8);
    s.push_str("{\"magic\":\"ai2048-td\",\"games\":");
    s.push_str(&games.to_string());
    s.push_str(",\"table\":[");
    for (i, x) in v.w.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{}", x));
    }
    s.push_str("]}");
    match std::fs::write(path, s) {
        Ok(()) => eprintln!("已保存 {}（{} 局）", path, games),
        Err(e) => eprintln!("保存 {} 失败: {}", path, e),
    }
}

fn main() {
    let mut games = 1000u64;
    let mut alpha = 0.001f32;
    let mut eps = 0.05f32;
    let mut out = String::from("weights.json");
    let mut resume: Option<String> = None;
    let mut seed = 1u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--games" => games = args.next().and_then(|v| v.parse().ok()).unwrap_or(1000),
            "--alpha" => alpha = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.001),
            "--eps" => eps = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.05),
            "--out" => out = args.next().unwrap_or_else(|| "weights.json".into()),
            "--resume" => resume = args.next(),
            "--seed" => seed = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
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
    if games == 0 {
        eprintln!("--games 必须大于 0\n\n{}", USAGE);
        std::process::exit(2);
    }

    let t = MoveTables::new();
    let mut v = match &resume {
        Some(p) => match Value::from_json(p) {
            Ok(x) => {
                eprintln!("从 {} 恢复训练", p);
                x
            }
            Err(e) => {
                eprintln!("恢复失败: {}（从头开始）", e);
                Value::new()
            }
        },
        None => Value::new(),
    };
    let mut rng = Rng::new(seed);

    let mut best = 0u64;
    let mut recent: Vec<u64> = Vec::new();
    let (mut n2048, mut n8192) = (0u64, 0u64);

    for gi in 1..=games {
        let mut b = 0u64;
        b = engine::spawn_tile(b, &mut rng);
        b = engine::spawn_tile(b, &mut rng);

        let mut score = 0u64;
        loop {
            let opts = options(&v, &t, b);
            if opts.is_empty() {
                break; // 死局
            }
            let eps_greedy = rng.below(1000) < (eps * 1000.0) as u64;
            let choice: Choice = if eps_greedy {
                let k = rng.below(opts.len() as u64) as usize;
                opts[k]
            } else {
                opts.into_iter()
                    .max_by(|a, c| a.3.partial_cmp(&c.3).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap()
            };
            let (after, gained, _, q_as) = choice;
            score += gained as u64;

            // 生成随机块得到 s'
            let s2 = engine::spawn_tile(after, &mut rng);
            let next = options(&v, &t, s2);
            let target = if next.is_empty() {
                gained as f32 // 死局：没有未来价值
            } else {
                gained as f32 + next.iter().map(|c| c.3).fold(f32::NEG_INFINITY, f32::max)
            };
            let delta = target - q_as;
            v.update(after, delta, alpha);

            b = s2;
            if !t.can_move(b) {
                break;
            }
        }

        best = best.max(score);
        recent.push(score);
        if recent.len() > 100 {
            recent.remove(0);
        }
        let mt = max_tile(b);
        if mt >= 11 {
            n2048 += 1;
        }
        if mt >= 13 {
            n8192 += 1;
        }

        if gi % 25 == 0 {
            let avg = recent.iter().sum::<u64>() as f64 / recent.len() as f64;
            eprintln!(
                "局 {:>6}  近100局平均 {:.0}  本局 {} (最高块 {})  最佳 {}  2048+ {} 局 / 8192+ {} 局",
                gi, avg, score, 1u64 << mt, best, n2048, n8192
            );
        }
        if gi % 500 == 0 {
            save(&out, &v, gi);
        }
    }

    save(&out, &v, games);
}
