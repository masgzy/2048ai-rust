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
        // 锁中毒恢复：TT 只是缓存，哪怕持锁线程曾 panic 也照常取用，
        // 否则一次 panic 会让之后所有请求永久失败（服务雪崩）。
        let tt = pool
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .unwrap_or_else(TT::new);
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
            let mut f = self.pool.free.lock().unwrap_or_else(|e| e.into_inner());
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
/// `dg` 为 true 时启用 tile-downgrading：根节点满足条件则先降级再搜索
/// （TDL2048+ 的 Tile-DG expectimax，见 `downgrade_root`）。
pub fn choose_move(
    pool: &TTPool,
    t: &MoveTables,
    ev: &Eval,
    board: u64,
    min_ms: u64,
    max_depth: u8,
) -> Result<MoveResult, String> {
    choose_move_ex(pool, t, ev, board, min_ms, max_depth, false)
}

pub fn choose_move_ex(
    pool: &TTPool,
    t: &MoveTables,
    ev: &Eval,
    board: u64,
    min_ms: u64,
    max_depth: u8,
    dg: bool,
) -> Result<MoveResult, String> {
    let root = if dg {
        let (d, applied) = downgrade_root(board);
        if applied {
            d
        } else {
            board
        }
    } else {
        board
    };
    let mut root_moves: Vec<(u8, u64)> = Vec::with_capacity(4);
    for dir in 0..4u8 {
        let (nb, _, moved) = t.move_board(root, dir as usize);
        if moved {
            root_moves.push((dir, nb));
        }
    }
    if root_moves.is_empty() {
        return Err("游戏结束：无路可走".to_string());
    }
    if root_moves.len() == 1 {
        let dir = root_moves[0].0;
        return Ok(MoveResult {
            dir,
            score: 0.0,
            depth: 0,
            nodes: 0,
            ms: 0,
            budget_ms: 0,
        });
    }

    let auto = auto_budget(board);
    let budget = if min_ms == 0 { auto } else { min_ms.max(auto) };
    let start = Instant::now();
    let deadline = start + Duration::from_millis(budget);
    // 自适应深度上限（nneonneo 式）：distinct_tiles - 2，最少 3 层；时间预算仍是硬闸
    let depth_cap = ((board::count_distinct(board).saturating_sub(2)).max(3) as u8).min(max_depth);

    // 各根着法线程把 (方向, 分值, 完成深度, 节点数) 写入同一个 Vec；节点数随结果一起收集，
    // join 之后再求和——不依赖 AtomicU64（ARMv5 等目标没有 64 位原子操作）。
    let results: Mutex<Vec<(u8, f32, u8, u64)>> = Mutex::new(Vec::new());
    let results = &results;

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
                results.lock().unwrap_or_else(|e| e.into_inner()).push((
                    dir,
                    best.0,
                    best.1,
                    ctx.nodes.get(),
                ));
            });
        }
    });

    let list = (*results.lock().unwrap_or_else(|e| e.into_inner())).clone();
    let nodes = list.iter().map(|r| r.3).sum::<u64>();
    let ms = start.elapsed().as_millis() as u64;
    let &(dir, score, depth, _) = list
        .iter()
        .max_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.2.cmp(&b.2))
        })
        .unwrap_or(&(0, 0.0, 0, 0));

    Ok(MoveResult {
        dir,
        score,
        depth,
        nodes,
        ms,
        budget_ms: budget,
    })
}

/// 无头对局（simulate 用）：完整打一局，返回统计。
pub struct GameOutcome {
    pub score: u64,
    pub max_rank: u16,
    pub moves: u64,
    pub nodes: u64,
    pub ms: u64,
}

/// Tile-downgrading（Guei et al. 2021，TDL2048+ 的 Tile-DG）：
/// 把根盘面所有大于"最大缺失块"的块减半，得到降级盘面后再搜索。
/// 论文条件（两个都满足才应用）：存在缺失块（如局面有 32768 而没有 256），
/// 且存在 32768 或更大的块。降级只在搜索根做一次，树内不再降级；
/// 降级盘面选出的着法直接作用于原盘面（滑动方向在减半映射下保持）。
///
/// 动机：n-tuple/行表评估对极大块状态训练不足、估值偏差大；
/// 降级后的状态与中小块状态分布重合，估值更准。
/// 返回 (降级盘面, 是否实际降级)。
pub fn downgrade_root(b: u64) -> (u64, bool) {
    let max = board::max_rank(b);
    // 条件二：最大块 >= 32768（rank 15）
    if max < 15 {
        return (b, false);
    }
    // 最大缺失块：不超过最大块的所有块中，不在盘面上的最大 rank
    // （论文：256-tile is the largest missing tile not exceeding the largest tile）
    let mut missing: u16 = 0;
    let mut present = [false; 16];
    for i in 0..16 {
        present[board::nibble_at(b, i) as usize] = true;
    }
    for r in (1..max).rev() {
        if !present[r as usize] {
            missing = r;
            break;
        }
    }
    // 条件一：存在缺失块
    if missing == 0 {
        return (b, false);
    }
    // 所有 > missing 的块减半（rank - 1）
    let mut d = b;
    for i in 0..16 {
        let v = board::nibble_at(d, i);
        if v > missing {
            d = (d & !(0xFu64 << (4 * i))) | (((v - 1) as u64) << (4 * i));
        }
    }
    (d, true)
}

pub fn play_game(
    pool: &TTPool,
    t: &MoveTables,
    ev: &Eval,
    min_ms: u64,
    max_depth: u8,
    rng: &mut board::Rng,
) -> GameOutcome {
    play_game_ex(pool, t, ev, min_ms, max_depth, rng, false)
}

pub fn play_game_ex(
    pool: &TTPool,
    t: &MoveTables,
    ev: &Eval,
    min_ms: u64,
    max_depth: u8,
    rng: &mut board::Rng,
    dg: bool,
) -> GameOutcome {
    let mut b = 0u64;
    for _ in 0..2 {
        b = spawn_tile(b, rng);
    }
    let (mut score, mut moves, mut nodes, mut ms) = (0u64, 0u64, 0u64, 0u64);
    while let Ok(r) = choose_move_ex(pool, t, ev, b, min_ms, max_depth, dg) {
        let (nb, gained, _) = t.move_board(b, r.dir as usize);
        score += gained as u64;
        nodes += r.nodes;
        ms += r.ms;
        moves += 1;
        b = spawn_tile(nb, rng);
    }
    GameOutcome {
        score,
        max_rank: board::max_rank(b),
        moves,
        nodes,
        ms,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downgrade_requires_32768_and_missing_tile() {
        // 条件不满足：没有 32768（rank 15）→ 不降级
        let b1 = board::set_nibble(0, 0, 14); // 16384
        assert_eq!(downgrade_root(b1), (b1, false));
        // 有 32768 且 1..14 全部在场（无缺失）→ 不降级
        let mut b2 = board::set_nibble(0, 0, 15); // idx0 = 32768
        for i in 1..=14usize {
            b2 = board::set_nibble(b2, i, i as u16); // idx1..14 = rank1..14
        }
        assert!(!downgrade_root(b2).1);
    }

    #[test]
    fn downgrade_halves_above_missing() {
        // 构造：32768(rank15) + 8192(rank13) + 1024(rank10) + 64(rank6)，
        // 缺失的最大块是 16384(rank14)？不对——15 在场，14 缺失 → missing=14，
        // 所有 >14 的块（只有 15）减半成 14。
        let mut b = board::set_nibble(0, 0, 15);
        b = board::set_nibble(b, 1, 13);
        b = board::set_nibble(b, 2, 10);
        let (d, applied) = downgrade_root(b);
        assert!(applied);
        assert_eq!(board::nibble_at(d, 0), 14); // 32768 → 16384
        assert_eq!(board::nibble_at(d, 1), 13); // 8192 不动（不 > missing=14）
        assert_eq!(board::nibble_at(d, 2), 10);
        // 论文图 10 的形态：最大块 32768、最大缺失 256 → 大于 256 的全部减半。
        // 构造：idx0=15(32768)；idx1..7 = 1..7（2..128，但 256/8 缺失）；
        // idx8..14 = 9..15；present = {1..7, 9..15}，missing = 8。
        let mut b2 = board::set_nibble(0, 0, 15);
        for i in 1..=14usize {
            let r = if i >= 8 { (i as u16) + 1 } else { i as u16 };
            b2 = board::set_nibble(b2, i, r);
        }
        let (d2, ok) = downgrade_root(b2);
        assert!(ok, "missing=8 应触发降级");
        // missing = 8（256 缺失），所有 rank>8 减半
        assert_eq!(board::nibble_at(d2, 0), 14); // idx0: 15→14
        assert_eq!(board::nibble_at(d2, 7), 7); // rank7 (128) 不动
        assert_eq!(board::nibble_at(d2, 8), 8); // idx8 原 rank9(512) → 8(256)
        assert_eq!(board::nibble_at(d2, 14), 14); // idx14 原 rank15→14
    }

    #[test]
    fn downgrade_dir_preserved() {
        // 降级只改块值不改布局：move_board 对降级盘面与原盘面应给出同方向合法着法集合
        let mut b = board::set_nibble(0, 0, 15);
        b = board::set_nibble(b, 5, 13);
        b = board::set_nibble(b, 10, 10);
        let (d, applied) = downgrade_root(b);
        assert!(applied);
        let t = MoveTables::new();
        for dir in 0..4 {
            let (_, _, m1) = t.move_board(b, dir);
            let (_, _, m2) = t.move_board(d, dir);
            assert_eq!(m1, m2, "方向 {} 的可动性应一致", dir);
        }
    }
}
