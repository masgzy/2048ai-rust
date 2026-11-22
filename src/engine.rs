//! expectimax 搜索引擎。
//!
//! 结构与 nneonneo 2048-AI 相同：
//! - 根节点 4 个方向各自开线程独立迭代加深（deadline 内跑多深算多深）；
//! - 玩家节点查置换表（TT），概率截断 PROB_MIN；
//! - 机会节点 90% 出 2 / 10% 出 4，被截断的概率质量用当前评估补回；
//! - 超时后产生的值不写入 TT（避免污染缓存）；
//! - 每线程独享 TT，TT 通过对象池复用，零搜索期分配。
//!
//! 思考预算完全自动：`min_ms == 0` 时按局面复杂度（空格数 / 最大块）自适应；
//! `min_ms > 0` 时仅作为“每步最短思考时间”下限（观看模式），自动预算仍然生效。

use std::cell::Cell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::board::{self, MoveTables};
use crate::eval::Eval;

/// 概率截断阈值（同 nneonneo CPROB_THRESH_BASE）
const PROB_MIN: f32 = 1e-4;
/// 每线程 TT 条目数的 log2（128K 条 ≈ 1.7MB）
const TT_BITS: usize = 17;

pub struct TT {
    keys: Vec<u64>,
    vals: Vec<f32>,
    deps: Vec<u8>,
}

impl TT {
    fn new() -> Self {
        let n = 1usize << TT_BITS;
        TT {
            keys: vec![0u64; n],
            vals: vec![0f32; n],
            deps: vec![0u8; n],
        }
    }

    #[inline]
    fn idx(key: u64) -> usize {
        (key.wrapping_mul(0x9E3779B97F4A7C15) >> (64 - TT_BITS)) as usize
    }

    /// 命中条件：同盘面且已缓存深度 >= 当前需要的深度。
    #[inline]
    fn get(&self, key: u64, depth: u8) -> Option<f32> {
        let i = Self::idx(key);
        if self.deps[i] >= depth && self.keys[i] == key {
            Some(self.vals[i])
        } else {
            None
        }
    }

    /// 更深的结果优先覆盖；浅结果不驱逐深结果。
    #[inline]
    fn put(&mut self, key: u64, depth: u8, val: f32) {
        let i = Self::idx(key);
        if self.keys[i] != key || self.deps[i] <= depth {
            self.keys[i] = key;
            self.vals[i] = val;
            self.deps[i] = depth;
        }
    }
}

/// TT 对象池：搜索期零分配，用完归还。
pub struct TTPool {
    free: Mutex<Vec<TT>>,
}

impl TTPool {
    pub fn new() -> Self {
        TTPool {
            free: Mutex::new((0..8).map(|_| TT::new()).collect()),
        }
    }
}

impl Default for TTPool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TTGuard<'a> {
    pool: &'a TTPool,
    tt: Option<TT>,
}

impl<'a> TTGuard<'a> {
    pub fn take(pool: &'a TTPool) -> Self {
        let tt = pool.free.lock().unwrap().pop().unwrap_or_else(TT::new);
        TTGuard { pool, tt: Some(tt) }
    }
}

impl Deref for TTGuard<'_> {
    type Target = TT;
    fn deref(&self) -> &TT {
        self.tt.as_ref().unwrap()
    }
}

impl DerefMut for TTGuard<'_> {
    fn deref_mut(&mut self) -> &mut TT {
        self.tt.as_mut().unwrap()
    }
}

impl Drop for TTGuard<'_> {
    fn drop(&mut self) {
        if let Some(tt) = self.tt.take() {
            let mut f = self.pool.free.lock().unwrap();
            if f.len() < 32 {
                f.push(tt);
            }
        }
    }
}

/// 每线程搜索上下文（不可变共享，超时/计数用 Cell）。
struct Ctx<'a> {
    t: &'a MoveTables,
    ev: &'a Eval,
    deadline: Instant,
    nodes: Cell<u64>,
    timed_out: Cell<bool>,
}

impl Ctx<'_> {
    #[inline]
    fn check_deadline(&self) -> bool {
        if self.timed_out.get() {
            return false;
        }
        if Instant::now() >= self.deadline {
            self.timed_out.set(true);
            return false;
        }
        true
    }

    /// 玩家节点：depth = 剩余可走步数（含本层）。
    fn player_node(&self, tt: &mut TT, b: u64, depth: u8, cprob: f32) -> f32 {
        self.nodes.set(self.nodes.get() + 1);
        if depth == 0 {
            return self.ev.eval(b);
        }
        if !self.check_deadline() {
            return self.ev.eval(b);
        }
        if let Some(v) = tt.get(b, depth) {
            return v;
        }
        // 死局返回 0（远低于任何活局的启发值，等价于死亡惩罚）
        let mut best = 0.0f32;
        let mut any = false;
        for dir in 0..4usize {
            let (nb, _, moved) = self.t.move_board(b, dir);
            if moved {
                any = true;
                let v = self.chance_node(tt, nb, depth - 1, cprob);
                if v > best {
                    best = v;
                }
            }
        }
        if any && !self.timed_out.get() {
            tt.put(b, depth, best);
        }
        best
    }

    /// 机会节点：depth = 生成随机块后剩余可走步数。
    /// 严格对齐 nneonneo 原版：入口处概率截断；对空格数取平均（`res / num_open`）。
    fn chance_node(&self, tt: &mut TT, b: u64, depth: u8, cprob: f32) -> f32 {
        self.nodes.set(self.nodes.get() + 1);
        if depth == 0 || cprob < PROB_MIN {
            return self.ev.eval(b);
        }
        let empties = board::count_empty(b);
        if empties == 0 {
            return self.ev.eval(b);
        }
        let cp = cprob / empties as f32;
        let mut acc = 0.0f32;
        for idx in 0..16usize {
            if board::nibble_at(b, idx) != 0 {
                continue;
            }
            for (rank, p) in [(1u16, 0.9f32), (2u16, 0.1f32)] {
                let nb = board::set_nibble(b, idx, rank);
                acc += p * self.player_node(tt, nb, depth, cp * p);
            }
        }
        // 关键：对空格数取平均。否则“移动后空格多”的方向被系统性放大，
        // 搜索退化为抢空格而非构建合并结构，棋力骤降。
        acc / empties as f32
    }
}

#[derive(Debug, Clone)]
pub struct MoveResult {
    pub dir: u8,
    pub score: f32,
    pub depth: u8,
    pub nodes: u64,
    pub ms: u64,
    pub budget_ms: u64,
}

/// 全自动思考预算：按空格数与最大块自适应（毫秒）。
/// 空格多 → 局面简单 → 快速出招；残局/大块 → 自动加时。
pub fn auto_budget(board: u64) -> u64 {
    let e = board::count_empty(board);
    let mt = board::max_rank(board);
    let mut ms = if e >= 12 {
        12
    } else if e >= 10 {
        18
    } else if e >= 8 {
        30
    } else if e >= 6 {
        55
    } else if e >= 4 {
        90
    } else if e >= 2 {
        130
    } else {
        180
    };
    if mt >= 11 {
        ms += 20; // 2048+
    }
    if mt >= 13 {
        ms += 40; // 8192+ 收官加时
    }
    ms
}

/// 为局面选着法。`min_ms == 0` 为纯自动预算；`min_ms > 0` 为每步最短思考时间下限。
pub fn choose_move(
    pool: &TTPool,
    t: &MoveTables,
    ev: &Eval,
    board: u64,
    min_ms: u64,
    max_depth: u8,
) -> Result<MoveResult, String> {
    let mut root_moves: Vec<(u8, u64)> = Vec::with_capacity(4);
    for dir in 0..4u8 {
        let (nb, _, moved) = t.move_board(board, dir as usize);
        if moved {
            root_moves.push((dir, nb));
        }
    }
    if root_moves.is_empty() {
        return Err("游戏结束：无路可走".to_string());
    }
    if root_moves.len() == 1 {
        let dir = root_moves[0].0;
        return Ok(MoveResult { dir, score: 0.0, depth: 0, nodes: 0, ms: 0, budget_ms: 0 });
    }

    let auto = auto_budget(board);
    let budget = if min_ms == 0 { auto } else { min_ms.max(auto) };
    let start = Instant::now();
    let deadline = start + Duration::from_millis(budget);
    // 自适应深度上限（nneonneo 式）：distinct_tiles - 2，最少 3 层；时间预算仍是硬闸
    let depth_cap = ((board::count_distinct(board).saturating_sub(2)).max(3) as u8).min(max_depth);

    let results: Mutex<Vec<(u8, f32, u8)>> = Mutex::new(Vec::new());
    let nodes_total = AtomicU64::new(0);
    let results = &results;
    let nodes_total = &nodes_total;

    std::thread::scope(|s| {
        for &(dir, nb) in &root_moves {
            s.spawn(move || {
                let mut tt = TTGuard::take(pool);
                let ctx = Ctx {
                    t,
                    ev,
                    deadline,
                    nodes: Cell::new(0),
                    timed_out: Cell::new(false),
                };
                let mut best = (0.0f32, 0u8);
                for d in 1..=depth_cap {
                    let v = ctx.chance_node(&mut tt, nb, d - 1, 1.0);
                    if ctx.timed_out.get() {
                        break; // 超时的半成品深度不采用
                    }
                    best = (v, d);
                    if d >= depth_cap {
                        break;
                    }
                    // 已用掉 60% 预算则下一层大概率跑不完，提前收手
                    if start.elapsed() >= Duration::from_millis(budget * 6 / 10) {
                        break;
                    }
                }
                nodes_total.fetch_add(ctx.nodes.get(), Ordering::Relaxed);
                results.lock().unwrap().push((dir, best.0, best.1));
            });
        }
    });

    let list = (*results.lock().unwrap()).clone();
    let nodes = nodes_total.load(Ordering::Relaxed);
    let ms = start.elapsed().as_millis() as u64;
    let &(dir, score, depth) = list
        .iter()
        .max_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.2.cmp(&b.2))
        })
        .unwrap_or(&(0, 0.0, 0));

    Ok(MoveResult { dir, score, depth, nodes, ms, budget_ms: budget })
}

/// 无头对局（simulate 用）：完整打一局，返回统计。
pub struct GameOutcome {
    pub score: u64,
    pub max_rank: u16,
    pub moves: u64,
    pub nodes: u64,
    pub ms: u64,
}

pub fn play_game(
    pool: &TTPool,
    t: &MoveTables,
    ev: &Eval,
    min_ms: u64,
    max_depth: u8,
    rng: &mut board::Rng,
) -> GameOutcome {
    let mut b = 0u64;
    for _ in 0..2 {
        b = spawn_tile(b, rng);
    }
    let (mut score, mut moves, mut nodes, mut ms) = (0u64, 0u64, 0u64, 0u64);
    loop {
        match choose_move(pool, t, ev, b, min_ms, max_depth) {
            Ok(r) => {
                let (nb, gained, _) = t.move_board(b, r.dir as usize);
                score += gained as u64;
                nodes += r.nodes;
                ms += r.ms;
                moves += 1;
                b = spawn_tile(nb, rng);
            }
            Err(_) => break,
        }
    }
    GameOutcome { score, max_rank: board::max_rank(b), moves, nodes, ms }
}

/// 随机生成一个块（90% 出 2，10% 出 4）。
pub fn spawn_tile(b: u64, rng: &mut board::Rng) -> u64 {
    let empties: Vec<usize> = (0..16).filter(|&i| board::nibble_at(b, i) == 0).collect();
    if empties.is_empty() {
        return b;
    }
    let idx = empties[rng.below(empties.len() as u64) as usize];
    let rank = if rng.below(10) < 9 { 1u16 } else { 2u16 };
    board::set_nibble(b, idx, rank)
}
