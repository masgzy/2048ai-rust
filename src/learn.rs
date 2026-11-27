//! OTD / OTC / OTD+TC 学习器（TDL2048+ 同族方法的零依赖 Rust 实现）。
//!
//! 方法出处：Hung Guei, Lung-Pin Chen, I-Chen Wu,
//! "Optimistic Temporal Difference Learning for 2048"（IEEE TG 2021,
//! arXiv:2111.11090），框架参考 moporgic/TDL2048（MIT）。要点：
//!
//! - **乐观初始化（OI）**：网络特征权重初始化为 `Vinit / m`（m = 元组数，此处 8），
//!   未访问状态的估值偏高，贪心策略会主动探索；被访问后估值回落，转而探索
//!   其它少访问状态（论文式 (12)，`LUT[i] <- Vinit/m`）。
//! - **TC（temporal coherence）**：每个权重维护 `E`（δ 累计）与 `A`（|δ| 累计），
//!   有效学习率按 `β = |E|/A` 调制（Jaśkowski 2015；论文式 (7)-(9)）——
//!   更新方向反复翻转的权重自动减速，一致方向的权重保持全速。
//! - **OTD+TC 两阶段**：先 OTD（固定 α，充分探索），后 TC 微调（快速收敛）。
//!   `Ptc` 为 TC 阶段占训练总量的比例（论文默认 10%）。
//! - **多阶段（multistage）**：整局按"本局最高块达到阈值"切阶段，各阶段
//!   独立权重表（TDL2048 的 `-@`），缓解大块状态训练不足。
//!
//! 本实现与 TDL2048+ 的差距（如实声明）：元组网络是 4x4 行列 8 元组
//! （等价 Yeh 4x6-tuple 的行列特例），不是 Matsuzaki 8x6-tuple；
//! 训练目标是"单机零依赖可用"，不追求论文 SOTA 配置（100M 局、
//! 8x6 网络、20 线程 SHM 无锁共享）。

use crate::board::{self, MoveTables};

/// 元组数：4 行 + 4 列共享一张 65536 项表。
pub const TUPLES: usize = 8;
/// 权重表大小（2^16 行编码）。
pub const TABLE: usize = 65536;

/// 单阶段网络：权重 + TC 统计（E/A）。
/// 内存：65536 × (4+4+4) = 768KB / 阶段。
#[derive(Clone)]
pub struct StageNet {
    pub w: Vec<f32>,
    pub e: Vec<f32>,
    pub a: Vec<f32>,
}

impl StageNet {
    fn zeroed() -> Self {
        StageNet {
            w: vec![0.0; TABLE],
            e: vec![0.0; TABLE],
            a: vec![0.0; TABLE],
        }
    }

    /// 乐观初始化：整网络值 Vinit 均摊到每个权重（论文式 (12)）。
    fn optimistic(vinit: f32) -> Self {
        let mut n = Self::zeroed();
        if vinit > 0.0 {
            n.w.fill(vinit / TUPLES as f32);
        }
        n
    }
}

/// 阶段判定：按"当前局面最大块 rank"。
/// 单阶段 = 阈值表为空；两阶段（默认）= 第二阶段从 rank >= 13（8192）起。
#[derive(Clone)]
pub struct Stages {
    /// 阶段起始 rank（升序，不含 0）。空 = 单阶段。
    pub thresholds: Vec<u16>,
    nets: Vec<StageNet>,
}

impl Stages {
    pub fn new(thresholds: Vec<u16>, vinit: f32) -> Self {
        let n = thresholds.len() + 1;
        Stages {
            thresholds,
            nets: (0..n).map(|_| StageNet::optimistic(vinit)).collect(),
        }
    }

    pub fn single(vinit: f32) -> Self {
        Self::new(vec![], vinit)
    }

    /// 当前阶段号：最高块越过几个阈值就在第几阶段。
    pub fn stage_of(&self, b: u64) -> usize {
        let mt = board::max_rank(b);
        let mut s = 0usize;
        for &t in &self.thresholds {
            if mt >= t {
                s += 1;
            }
        }
        s
    }

    pub fn stage_count(&self) -> usize {
        self.nets.len()
    }

    /// V(board) = Σ 8 元组查表（4 行 + 4 列）。
    pub fn v(&self, b: u64) -> f32 {
        let net = &self.nets[self.stage_of(b)];
        let t = board::transpose(b);
        let w = &net.w;
        (w[(b & 0xFFFF) as usize]
            + w[((b >> 16) & 0xFFFF) as usize]
            + w[((b >> 32) & 0xFFFF) as usize]
            + w[((b >> 48) & 0xFFFF) as usize])
            + (w[(t & 0xFFFF) as usize]
                + w[((t >> 16) & 0xFFFF) as usize]
                + w[((t >> 32) & 0xFFFF) as usize]
                + w[((t >> 48) & 0xFFFF) as usize])
    }

    /// afterstate 的 8 个元组索引。
    #[inline]
    fn tuple_indices(b: u64) -> [usize; 8] {
        let t = board::transpose(b);
        [
            (b & 0xFFFF) as usize,
            ((b >> 16) & 0xFFFF) as usize,
            ((b >> 32) & 0xFFFF) as usize,
            ((b >> 48) & 0xFFFF) as usize,
            (t & 0xFFFF) as usize,
            ((t >> 16) & 0xFFFF) as usize,
            ((t >> 32) & 0xFFFF) as usize,
            ((t >> 48) & 0xFFFF) as usize,
        ]
    }

    /// TD(0) 更新（afterstate，均摊 δ/8）。
    pub fn update_td(&mut self, b: u64, delta: f32, alpha: f32) {
        let stage = self.stage_of(b);
        let net = &mut self.nets[stage];
        let d = alpha * delta / TUPLES as f32;
        for i in Self::tuple_indices(b) {
            net.w[i] += d;
        }
    }

    /// TC 更新：β = |E|/A 调制（Jaśkowski；论文式 (7)-(9)）。
    /// δ 累计进 E/A，调整量 = α·β·δ/8。A=0 时 β=1（首个更新全速）。
    pub fn update_tc(&mut self, b: u64, delta: f32, alpha: f32) {
        let stage = self.stage_of(b);
        let net = &mut self.nets[stage];
        let d = alpha * delta / TUPLES as f32;
        for i in Self::tuple_indices(b) {
            // 先更新统计再取 β（与 TDL2048 的顺序一致：本轮 δ 计入 E/A）
            net.e[i] += delta;
            net.a[i] += delta.abs();
            let beta = if net.a[i] > 0.0 {
                (net.e[i].abs() / net.a[i]).min(1.0)
            } else {
                1.0
            };
            net.w[i] += beta * d;
        }
    }

    /// 逐元素平均合并（多线程训练的权重聚合）。
    pub fn merge_from(&mut self, other: &Stages, weight: f32) {
        for (mine, theirs) in self.nets.iter_mut().zip(other.nets.iter()) {
            for (m, &o) in mine.w.iter_mut().zip(theirs.w.iter()) {
                *m += weight * o;
            }
        }
    }

    pub fn scale(&mut self, k: f32) {
        for net in &mut self.nets {
            for w in net.w.iter_mut() {
                *w *= k;
            }
        }
    }

    /// 序列化：{"v": 常量, "stages": 阈值数, "tables": [[...], ...]}
    pub fn to_json_parts(&self) -> (usize, Vec<String>) {
        let tables: Vec<String> = self
            .nets
            .iter()
            .map(|net| {
                let mut s = String::with_capacity(TABLE * 8);
                s.push('[');
                for (i, x) in net.w.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(&format!("{:.4}", x));
                }
                s.push(']');
                s
            })
            .collect();
        (self.thresholds.len(), tables)
    }

    /// 从 JSON 值恢复（load_file 的辅助）。
    pub fn from_tables(thresholds: Vec<u16>, tables: &[Vec<f32>]) -> Option<Self> {
        if tables.is_empty() || tables.len() != thresholds.len() + 1 {
            return None;
        }
        let mut nets = Vec::with_capacity(tables.len());
        for t in tables {
            if t.len() != TABLE {
                return None;
            }
            let mut n = StageNet::zeroed();
            n.w = t.to_vec();
            nets.push(n);
        }
        Some(Stages { thresholds, nets })
    }
}

/// 学习模式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 乐观 TD（固定 α）
    Otd,
    /// 乐观 TC（α 通常 1.0，β 自适应调制）
    Otc,
    /// 先 OTD 后 TC 两阶段（Ptc = TC 占比）
    OtdTc,
}

/// 一局自博弈：返回 (局分, 最大 rank, 步数)。
/// 学习更新直接写进 `nets`（调用方保证单线程独占）。
pub fn play_and_learn(
    nets: &mut Stages,
    t: &MoveTables,
    rng: &mut board::Rng,
    mode: Mode,
    alpha: f32,
    eps: f32,
) -> (u64, u16, u64) {
    let mut b = 0u64;
    b = crate::engine::spawn_tile(b, rng);
    b = crate::engine::spawn_tile(b, rng);

    let mut score = 0u64;
    let mut moves = 0u64;
    loop {
        // 枚举 afterstate 与估值
        let mut opts: [(u64, u32, f32); 4] = [(0, 0, 0.0); 4];
        let mut n = 0usize;
        for dir in 0..4usize {
            let (nb, gained, moved) = t.move_board(b, dir);
            if moved {
                opts[n] = (nb, gained, nets.v(nb));
                n += 1;
            }
        }
        if n == 0 {
            break; // 死局
        }
        // ε-greedy（OI 提供隐式探索，ε 缺省很小）
        let pick = if rng.below(1000) < (eps * 1000.0) as u64 {
            rng.below(n as u64) as usize
        } else {
            (0..n)
                .max_by(|&x, &y| {
                    opts[x]
                        .2
                        .partial_cmp(&opts[y].2)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(0)
        };
        let (after, gained, q_as) = opts[pick];
        score += gained as u64;
        moves += 1;

        // s' = afterstate + 随机块；target = r + max_a' V(after(s',a'))，死局则 r
        let s2 = crate::engine::spawn_tile(after, rng);
        let mut next_best = f32::NEG_INFINITY;
        let mut alive = false;
        for dir in 0..4usize {
            let (nb, _, moved) = t.move_board(s2, dir);
            if moved {
                alive = true;
                let v = nets.v(nb);
                if v > next_best {
                    next_best = v;
                }
            }
        }
        let target = if alive {
            gained as f32 + next_best
        } else {
            gained as f32
        };
        let delta = target - q_as;

        match mode {
            Mode::Otd => nets.update_td(after, delta, alpha),
            Mode::Otc | Mode::OtdTc => nets.update_tc(after, delta, alpha),
        }

        b = s2;
        if !t.can_move(b) {
            break;
        }
    }
    (score, board::max_rank(b), moves)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimistic_init_distributed() {
        let s = Stages::single(80_000.0);
        // Vinit/m 均摊到每个权重
        assert!((s.nets[0].w[0] - 80_000.0 / 8.0).abs() < 1e-3);
        // 空盘估值 = 8 × Vinit/8 = Vinit（16 个 0 行全查 w[0]）
        // 注意：空盘所有行索引都是 0 → V = 8*w[0] = Vinit
        assert!((s.v(0) - 80_000.0).abs() < 1.0);
    }

    #[test]
    fn stage_routing() {
        let mut s = Stages::new(vec![13], 0.0); // 8192 起进第二阶段
        assert_eq!(s.stage_count(), 2);
        let small = board::set_nibble(0, 0, 11); // 2048
        let big = board::set_nibble(0, 0, 13); // 8192
        assert_eq!(s.stage_of(small), 0);
        assert_eq!(s.stage_of(big), 1);
        // 估值走不同表：手动改第二阶段 w[0] 应只影响大块盘面
        s.nets[1].w[0] = 1.0;
        assert_eq!(s.v(small), 0.0);
        // big 的 8 次查表中 6 次落在 w[0]（三个空行 + 三个空列），w[13]=0
        assert!((s.v(big) - 6.0).abs() < 1e-6);
    }

    #[test]
    fn tc_beta_modulates() {
        let mut s = Stages::single(0.0);
        let b = board::set_nibble(0, 0, 1);
        // 一致方向更新：β 应接近 1
        for _ in 0..10 {
            s.update_tc(b, 1.0, 0.1);
        }
        let w_consistent = s.nets[0].w[0];
        // 反复翻转：β 应显著减小（E 抵消，A 累加）
        let mut s2 = Stages::single(0.0);
        for i in 0..10 {
            let d = if i % 2 == 0 { 1.0 } else { -1.0 };
            s2.update_tc(b, d, 0.1);
        }
        assert!(
            s2.nets[0].w[0].abs() < w_consistent,
            "翻转更新的权重增量 ({}) 应小于一致方向 ({})",
            s2.nets[0].w[0],
            w_consistent
        );
    }

    #[test]
    fn td_update_splits_delta() {
        let mut s = Stages::single(0.0);
        let b = board::set_nibble(0, 0, 1);
        s.update_td(b, 8.0, 1.0);
        // δ=8 均摊到 8 个元组，每元组 +1。b 的索引表 = [1,0,0,0, 1,0,0,0]
        // （行0=1，行1..3=0；转置同形）：w[0] 被加 6 次 = 6，w[1] 加 2 次 = 2
        assert!(
            (s.nets[0].w[0] - 6.0).abs() < 1e-5,
            "w[0]={}",
            s.nets[0].w[0]
        );
        assert!(
            (s.nets[0].w[1] - 2.0).abs() < 1e-5,
            "w[1]={}",
            s.nets[0].w[1]
        );
    }

    #[test]
    fn merge_averages() {
        let mut a = Stages::single(0.0);
        let mut bnet = Stages::single(0.0);
        a.nets[0].w[0] = 8.0;
        bnet.nets[0].w[0] = 4.0;
        // 平均：a = (a + b) / 2 → w = 6（train 多线程合并同此式）
        a.merge_from(&bnet, 1.0);
        a.scale(0.5);
        assert!((a.nets[0].w[0] - 6.0).abs() < 1e-5);
    }

    #[test]
    fn play_and_learn_runs() {
        // 冒烟：小训练不 panic，且权重发生变化
        let t = MoveTables::new();
        let mut s = Stages::single(80_000.0);
        let mut rng = board::Rng::new(42);
        let before = s.nets[0].w[12345];
        for _ in 0..3 {
            play_and_learn(&mut s, &t, &mut rng, Mode::OtdTc, 0.1, 0.01);
        }
        assert!(s.nets[0].w.iter().any(|&w| w != 80_000.0 / 8.0));
        let _ = before;
    }
}
