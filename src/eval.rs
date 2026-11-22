//! 评估函数。
//!
//! 两种来源：
//! 1. 内置启发式（nneonneo 2048-AI 同款）：单调性 / 空格 / 可合并 / 数值和，
//!    预计算为 65536 项行表，评估整盘 = 4 行 + 4 列（转置后）共 8 次查表；
//! 2. TD 强化学习权重（`train` 子命令产出、`weights.json` 加载）：
//!    评估 = Σ 8 个元组（4 行 + 4 列）查同一张 65536 项表。
//!
//! 两者接口一致：值越大越好；死局由搜索层返回 0 处理。

use crate::board::{self, nibble};
use crate::json::Json;

#[derive(Debug, Clone)]
pub struct Weights {
    pub mono_w: f32,
    pub mono_power: f32,
    pub sum_w: f32,
    pub sum_power: f32,
    pub merges_w: f32,
    pub empty_w: f32,
    pub lost_penalty: f32,
}

impl Default for Weights {
    fn default() -> Self {
        // nneonneo 2048-AI 的经验权重
        Weights {
            mono_w: 47.0,
            mono_power: 4.0,
            sum_w: 11.0,
            sum_power: 3.5,
            merges_w: 700.0,
            empty_w: 270.0,
            lost_penalty: 200000.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalKind {
    Builtin,
    TD,
}

#[derive(Debug, Clone)]
pub struct Eval {
    pub heur: Vec<f32>, // 65536
    pub kind: EvalKind,
}

fn heur_row(row: u16, w: &Weights) -> f32 {
    let line = [nibble(row, 0), nibble(row, 1), nibble(row, 2), nibble(row, 3)];
    let mut sum = 0.0f32;
    let (mut empty, mut merges) = (0i32, 0i32);
    let (mut prev, mut counter) = (0u16, 0i32);
    for &rank in &line {
        sum += (rank as f32).powf(w.sum_power);
        if rank == 0 {
            empty += 1;
        } else {
            if prev == rank {
                counter += 1;
            } else if counter > 0 {
                merges += 1 + counter;
                counter = 0;
            }
        }
        prev = rank;
    }
    if counter > 0 {
        merges += 1 + counter;
    }
    let (mut mono_left, mut mono_right) = (0.0f32, 0.0f32);
    for i in 0..3 {
        let d = line[i] as f32 - line[i + 1] as f32;
        if d > 0.0 {
            mono_left += d.powf(w.mono_power);
        } else {
            mono_right += (-d).powf(w.mono_power);
        }
    }
    w.lost_penalty + w.empty_w * empty as f32 + w.merges_w * merges as f32
        - w.mono_w * mono_left.min(mono_right)
        - w.sum_w * sum
}

impl Eval {
    pub fn build(w: &Weights) -> Self {
        let mut heur = vec![0f32; 65536];
        for row in 0..=65535u32 {
            heur[row as usize] = heur_row(row as u16, w);
        }
        Eval { heur, kind: EvalKind::Builtin }
    }

    /// 从 TD 权重直接构造（值越大越好，符号不限）。
    pub fn from_td_table(table: Vec<f32>) -> Option<Self> {
        if table.len() != 65536 {
            return None;
        }
        Some(Eval { heur: table, kind: EvalKind::TD })
    }

    /// 整盘评估：4 行 + 4 列（转置后）共 8 次查表。
    #[inline]
    pub fn eval(&self, b: u64) -> f32 {
        let t = board::transpose(b);
        let h = &self.heur;
        (h[(b & 0xFFFF) as usize]
            + h[((b >> 16) & 0xFFFF) as usize]
            + h[((b >> 32) & 0xFFFF) as usize]
            + h[((b >> 48) & 0xFFFF) as usize])
            + (h[(t & 0xFFFF) as usize]
                + h[((t >> 16) & 0xFFFF) as usize]
                + h[((t >> 32) & 0xFFFF) as usize]
                + h[((t >> 48) & 0xFFFF) as usize])
    }

    /// 单行评估（调试/测试用）。
    pub fn row_score(&self, row: u16) -> f32 {
        self.heur[row as usize]
    }
}

/// 从 `weights.json` 加载 TD 权重表。
/// 期望格式：{"magic":"ai2048-td","games":N,"table":[65536 个数]}
pub fn load_file(path: &str) -> Result<Eval, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败: {}", path, e))?;
    let j = Json::parse(&text).map_err(|e| format!("{} 不是合法 JSON: {}", path, e))?;
    let magic = j.get("magic").and_then(|v| v.as_str()).unwrap_or("");
    if magic != "ai2048-td" {
        return Err(format!("{} 缺少 magic=ai2048-td 标记", path));
    }
    let arr = j
        .get("table")
        .and_then(|v| v.as_arr())
        .ok_or_else(|| format!("{} 缺少 table 数组", path))?;
    if arr.len() != 65536 {
        return Err(format!("{} 的 table 长度应为 65536，实际 {}", path, arr.len()));
    }
    let table: Vec<f32> = arr.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
    Eval::from_td_table(table).ok_or_else(|| "table 长度错误".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_prefers_monotonic() {
        let ev = Eval::build(&Weights::default());
        // 行 [1,2,3,4]（递增）与 [2,1,4,3]（乱序）：空格/合并/数值和完全相同，
        // 唯一差别是单调性惩罚 → 递增行必须更高
        assert!(ev.row_score(0x1234) > ev.row_score(0x2143));
        assert!(ev.row_score(0x4321) > ev.row_score(0x3412));
    }

    #[test]
    fn eval_symmetric_rows_and_cols() {
        let ev = Eval::build(&Weights::default());
        // 单块 2：位于 (0,0) 与转置位置 (0,0)→(0,0)；验证 (1,2) 与 (2,1) 等价
        let b1 = board::set_nibble(0, 4 * 1 + 2, 1);
        let b2 = board::set_nibble(0, 4 * 2 + 1, 1);
        assert_eq!(ev.eval(b1), ev.eval(b2));
    }
}
