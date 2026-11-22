//! 2048 位棋盘。
//!
//! 表示：`u64`，每行占 16 位，每格 4 位存 log2 值（0=空，1=2，2=4，…，15=32768）。
//! 行 `r` 占据 bit `16r .. 16r+15`；行内格子 `c`（0=左）占据该 16 位的低 4 位起第 `c` 个 nibble。
//! 即 nibble 序号 = `4*r + c`，与 nneonneo 2048-AI 的布局一致。
//!
//! 方向编码（与前端一致）：0=左，1=右，2=上，3=下。

/// 方向：左
pub const DIR_LEFT: usize = 0;
/// 方向：右
pub const DIR_RIGHT: usize = 1;
/// 方向：上
pub const DIR_UP: usize = 2;
/// 方向：下
pub const DIR_DOWN: usize = 3;

#[inline]
pub fn nibble(row: u16, c: usize) -> u16 {
    (row >> (4 * c)) & 0xF
}

#[inline]
pub fn get_row(b: u64, r: usize) -> u16 {
    ((b >> (16 * r)) & 0xFFFF) as u16
}

#[inline]
pub fn set_row(b: u64, r: usize, v: u16) -> u64 {
    (b & !(0xFFFFu64 << (16 * r))) | ((v as u64) << (16 * r))
}

/// nibble 序号 idx（0..16）写入 rank
#[inline]
pub fn set_nibble(b: u64, idx: usize, rank: u16) -> u64 {
    b | ((rank as u64) << (4 * idx))
}

#[inline]
pub fn nibble_at(b: u64, idx: usize) -> u16 {
    ((b >> (4 * idx)) & 0xF) as u16
}

/// 4x4 nibble 转置（位技巧，与朴素实现等价——见单元测试）。
#[inline]
pub fn transpose(x: u64) -> u64 {
    let a1 = x & 0xF0F00F0FF0F00F0F;
    let a2 = x & 0x0000F0F00000F0F0;
    let a3 = x & 0x0F0F00000F0F0000;
    let a = a1 | (a2 << 12) | (a3 >> 12);
    let b1 = a & 0xFF00FF0000FF00FF;
    let b2 = a & 0x00FF00FF00000000;
    let b3 = a & 0x00000000FF00FF00;
    b1 | (b2 >> 24) | (b3 << 24)
}

#[cfg(test)]
fn transpose_naive(x: u64) -> u64 {
    let mut r = 0u64;
    for i in 0..4 {
        for j in 0..4 {
            let v = nibble_at(x, 4 * i + j);
            r = set_nibble(r, 4 * j + i, v);
        }
    }
    r
}

/// 单行滑动（朝 index 0 方向压缩合并），返回（新行, 得分）。
pub fn slide_line(line: &[u16; 4], to_right: bool) -> ([u16; 4], u32) {
    let mut tiles: Vec<u16> = line.iter().copied().filter(|&r| r != 0).collect();
    if to_right {
        tiles.reverse();
    }
    let mut out = [0u16; 4];
    let mut score = 0u32;
    let (mut i, mut o) = (0usize, 0usize);
    while i < tiles.len() {
        if i + 1 < tiles.len() && tiles[i] == tiles[i + 1] {
            let merged = (tiles[i] + 1).min(15);
            out[o] = merged;
            score += 1u32 << merged;
            o += 1;
            i += 2;
        } else {
            out[o] = tiles[i];
            o += 1;
            i += 1;
        }
    }
    if to_right {
        out.reverse();
    }
    (out, score)
}

fn pack(line: &[u16; 4]) -> u16 {
    line[0] | (line[1] << 4) | (line[2] << 8) | (line[3] << 12)
}

/// 65536 项预计算行表：左/右滑动结果与得分。
pub struct MoveTables {
    pub left: Vec<u16>,
    pub right: Vec<u16>,
    pub score_left: Vec<u32>,
    pub score_right: Vec<u32>,
}

impl MoveTables {
    pub fn new() -> Self {
        let mut t = MoveTables {
            left: vec![0u16; 65536],
            right: vec![0u16; 65536],
            score_left: vec![0u32; 65536],
            score_right: vec![0u32; 65536],
        };
        for row in 0..=65535u32 {
            let line = [
                nibble(row as u16, 0),
                nibble(row as u16, 1),
                nibble(row as u16, 2),
                nibble(row as u16, 3),
            ];
            let (l, sl) = slide_line(&line, false);
            let (r, sr) = slide_line(&line, true);
            t.left[row as usize] = pack(&l);
            t.right[row as usize] = pack(&r);
            t.score_left[row as usize] = sl;
            t.score_right[row as usize] = sr;
        }
        t
    }

    #[inline]
    fn rows(&self, b: u64, left: bool) -> (u64, u32, bool) {
        let (mut res, mut sc, mut moved) = (0u64, 0u32, false);
        for r in 0..4 {
            let row = get_row(b, r);
            let (nr, s) = if left {
                (self.left[row as usize], self.score_left[row as usize])
            } else {
                (self.right[row as usize], self.score_right[row as usize])
            };
            sc += s;
            if nr != row {
                moved = true;
            }
            res = set_row(res, r, nr);
        }
        (res, sc, moved)
    }

    /// 整盘移动：返回（新盘面，本步得分，是否移动）。
    #[inline]
    pub fn move_board(&self, b: u64, dir: usize) -> (u64, u32, bool) {
        match dir {
            DIR_LEFT => self.rows(b, true),
            DIR_RIGHT => self.rows(b, false),
            DIR_UP => {
                let r = self.rows(transpose(b), true);
                (transpose(r.0), r.1, r.2)
            }
            _ => {
                let r = self.rows(transpose(b), false);
                (transpose(r.0), r.1, r.2)
            }
        }
    }

    /// 是否存在任何合法移动。
    pub fn can_move(&self, b: u64) -> bool {
        (0..4).any(|d| self.move_board(b, d).2)
    }
}

impl Default for MoveTables {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
pub fn count_empty(b: u64) -> u32 {
    let mut n = 0;
    for i in 0..16 {
        if nibble_at(b, i) == 0 {
            n += 1;
        }
    }
    n
}

#[inline]
pub fn max_rank(b: u64) -> u16 {
    let mut m = 0u16;
    for i in 0..16 {
        let v = nibble_at(b, i);
        if v > m {
            m = v;
        }
    }
    m
}

/// 盘面上不同种类方块数（nneonneo 用于自适应深度上限）。
#[inline]
pub fn count_distinct(b: u64) -> u32 {
    let mut mask = 0u32;
    for i in 0..16 {
        let r = nibble_at(b, i);
        if r != 0 {
            mask |= 1 << r;
        }
    }
    mask.count_ones()
}

/// 从 4x4 实际牌值数组构造位棋盘（带校验：必须为 2 的幂且 <= 32768）。
pub fn from_array(arr: &[[u32; 4]; 4]) -> Result<u64, String> {
    let mut b = 0u64;
    for r in 0..4 {
        for c in 0..4 {
            let v = arr[r][c];
            if v == 0 {
                continue;
            }
            if !v.is_power_of_two() || v > 32768 {
                return Err(format!("非法方块值 {}（必须为 2 的幂且 <= 32768）", v));
            }
            b = set_nibble(b, 4 * r + c, v.trailing_zeros() as u16);
        }
    }
    Ok(b)
}

/// 位棋盘还原为 4x4 实际牌值数组。
pub fn to_array(b: u64) -> [[u32; 4]; 4] {
    let mut arr = [[0u32; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            let rank = nibble_at(b, 4 * r + c);
            arr[r][c] = if rank == 0 { 0 } else { 1u32 << rank };
        }
    }
    arr
}

/// xorshift64* 随机数（无第三方依赖）。
pub struct Rng {
    s: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { s: seed | 1 }
    }

    pub fn from_time() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
        let p = &t as *const u64 as u64;
        Rng::new(t ^ p.rotate_left(17))
    }

    #[inline]
    pub fn next(&mut self) -> u64 {
        let mut x = self.s;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.s = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_move(g0: &[[u16; 4]; 4], dir: usize) -> ([[u16; 4]; 4], u32, bool) {
        let mut g = *g0;
        let mut score = 0u32;
        let mut moved = false;
        let mut merged = [[false; 4]; 4];
        let (dx, dy): (i32, i32) = match dir {
            0 => (-1, 0),
            1 => (1, 0),
            2 => (0, -1),
            _ => (0, 1),
        };
        let mut order: Vec<(usize, usize)> = Vec::with_capacity(16);
        for y in 0..4 {
            for x in 0..4 {
                order.push((x, y));
            }
        }
        // 优先处理靠目标边的格子
        order.sort_by_key(|&(x, y)| match dir {
            0 => x as i32,
            1 => -(x as i32),
            2 => y as i32,
            _ => -(y as i32),
        });
        for &(x0, y0) in &order {
            let mut val = g[y0][x0];
            if val == 0 {
                continue;
            }
            let (mut cx, mut cy) = (x0 as i32, y0 as i32);
            g[y0][x0] = 0;
            loop {
                let (nx, ny) = (cx + dx, cy + dy);
                if !(0..4).contains(&nx) || !(0..4).contains(&ny) {
                    break;
                }
                let nv = g[ny as usize][nx as usize];
                if nv == 0 {
                    cx = nx;
                    cy = ny;
                    continue;
                }
                if nv == val && !merged[ny as usize][nx as usize] {
                    g[ny as usize][nx as usize] = val + 1;
                    merged[ny as usize][nx as usize] = true;
                    score += 1u32 << (val + 1);
                    moved = true;
                    val = 0;
                    break;
                }
                break;
            }
            if val != 0 {
                if (cx, cy) != (x0 as i32, y0 as i32) {
                    moved = true;
                }
                g[cy as usize][cx as usize] = val;
            }
        }
        (g, score, moved)
    }

    fn grid_to_board(g: &[[u16; 4]; 4]) -> u64 {
        let mut b = 0u64;
        for r in 0..4 {
            for c in 0..4 {
                b = set_nibble(b, 4 * r + c, g[r][c]);
            }
        }
        b
    }

    #[test]
    fn transpose_matches_naive() {
        let mut rng = Rng::new(42);
        for _ in 0..1000 {
            let mut b = 0u64;
            for i in 0..16 {
                b = set_nibble(b, i, (rng.next() % 16) as u16);
            }
            assert_eq!(transpose(b), transpose_naive(b));
        }
    }

    #[test]
    fn slide_basics() {
        let (l, s) = slide_line(&[1, 1, 1, 0], false);
        assert_eq!((l, s), ([2, 1, 0, 0], 4));
        let (l, s) = slide_line(&[2, 2, 3, 3], false);
        assert_eq!((l, s), ([3, 4, 0, 0], 8 + 16));
        let (r, s) = slide_line(&[1, 1, 1, 0], true);
        assert_eq!((r, s), ([0, 0, 1, 2], 4));
        let (l, s) = slide_line(&[0, 2, 0, 2], false);
        assert_eq!((l, s), ([3, 0, 0, 0], 8));
    }

    #[test]
    fn moves_match_naive() {
        let t = MoveTables::new();
        let mut rng = Rng::new(7);
        // 测试盘面 rank 限制在 0..=14：两个 32768（rank15）合并会溢出 nibble，真实对局不可达
        for _ in 0..500 {
            let mut g = [[0u16; 4]; 4];
            for r in 0..4 {
                for c in 0..4 {
                    g[r][c] = (rng.next() % 15) as u16;
                }
            }
            let b = grid_to_board(&g);
            for dir in 0..4usize {
                let (nb, sc, mv) = t.move_board(b, dir);
                let (ng, nsc, nmv) = naive_move(&g, dir);
                let expect = grid_to_board(&ng);
                assert_eq!(nb, expect, "dir={} board={:016x}", dir, b);
                assert_eq!(sc, nsc);
                assert_eq!(mv, nmv);
            }
        }
    }

    #[test]
    fn up_moves_to_top() {
        let t = MoveTables::new();
        // 只有左下角一个 2（rank 1）
        let b = set_nibble(0, 4 * 3, 1);
        let (nb, _, mv) = t.move_board(b, DIR_UP);
        assert!(mv);
        assert_eq!(nibble_at(nb, 0), 1); // 到达左上角
    }

    #[test]
    fn array_roundtrip() {
        let arr = [[2u32, 0, 4, 8], [0, 1024, 0, 32768], [2, 2, 2, 2], [0, 0, 0, 65536 >> 1]];
        let b = from_array(&arr).unwrap();
        let back = to_array(b);
        assert_eq!(arr, back);
        assert!(from_array(&[[3u32, 0, 0, 0], [0; 4], [0; 4], [0; 4]]).is_err());
        assert!(from_array(&[[65536u32, 0, 0, 0], [0; 4], [0; 4], [0; 4]]).is_err());
    }
}
