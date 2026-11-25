# 2048ai-rust

使用 Rust 编写的高性能 2048 自动化通关程序：**expectimax 搜索 + 64 位棋盘（bitboard）+ 置换表 + 概率剪枝**，并内置全自动多线程基准测试。

English intro: a high-performance, dependency-free 2048 AI in Rust — expectimax search over a 64-bit bitboard with a transposition table and probability pruning, plus a fully automated multi-threaded benchmark harness.

## 性能 / Performance

实测于 2 核虚拟机（本仓库 CI 同量级硬件），release 构建：

| 指标 | 数值 |
| --- | --- |
| 搜索吞吐（单线程，强度 3） | **8,350 万节点/秒** |
| 搜索吞吐（2 线程聚合） | **1.23 亿节点/秒** |
| 自动对局（强度 3，6 局） | **6/6 全部合成 8192**（2048/4096/8192 达成率 100%） |
| 单局到 8192 | 平均 3,749 步 / ~50 秒 |
| 完整对局（强度 2，直到死局） | 合成 **16384**，得分 271,380，10,303 步，14.2 秒 |
| 单局到 2048（强度 3） | 1,002 步 / 10.3 秒 |

"下一层级合成"验证：相对游戏目标 2048，本 AI 在默认强度下即可稳定合成 **4096 / 8192**，低强度完整对局即可达到 **16384**（棋盘编码支持到 32768）。

## 特性 / Features

- **极速引擎**：棋盘为单个 `u64`（每格 4 bit），四个方向的移动、得分、启发式评估全部查表完成（65536 项预计算表），单步移动仅 4 次查表 + 少量位运算。
- **expectimax 搜索**：己方节点取最大、随机节点按 90%（出 2）/ 10%（出 4）加权期望；按路径概率剪枝，密集局面自动深搜、稀疏局面自动浅搜。
- **置换表**：2^22 项直映射缓存（64 MiB/线程），同一局面跨步复用。
- **启发式评估**：空格数、可合并对、单调性、大数惩罚（经典调参，行/列各 4 次查表求和）。
- **7 档强度**：从 ~0.1 ms/步 到 ~1 s/步，见 `--strength 1..=7`。
- **全自动基准**：`bench` 多线程并行自对局并汇总统计（达成率、吞吐、每局报告）；`speed` 测量纯搜索吞吐（节点/秒）。
- **可复现**：PCG32 种子随机数，同种子对局逐位一致；`bestmove` 子命令输出指定局面的最佳走法，便于外部集成（stdin 16 个数值或 hex 棋盘）。
- **零第三方依赖、零 `unsafe`**：供应链攻击面为零；全部逻辑（RNG、CLI、JSON 输出）在本仓库内实现并测试。

## 构建 / Build

```bash
cargo build --release
# 二进制位于 target/release/2048ai
```

## 用法 / Usage

```bash
# 观看 AI 自动通关一局（终端彩色渲染）
2048ai play --seed 7

# 自动化基准：8 局、双线程、强度 3，统计到 4096 的达成率
2048ai bench --games 8 --threads 2 --strength 3 --goal 4096

# 纯搜索吞吐测试（节点/秒）
2048ai speed --secs 5 --strength 3

# 外部集成：读入局面，输出最佳走法（UP/DOWN/LEFT/RIGHT）
echo "2 0 0 0  0 0 0 0  0 0 0 0  0 0 0 4" | 2048ai bestmove

# 所有命令支持 --json 机器可读输出；2048ai help 查看完整选项
```

## 算法 / Algorithm

- 搜索框架为 expectimax（对随机玩家取期望、对自己取最大），配合：
  1. **概率剪枝**：路径累计概率低于阈值（按强度 1e-2 … 1e-4）的分支直接用启发式评估截断；
  2. **自适应深度**：依局面复杂度（不同方块种类数）与拥挤程度在 2…12 层间调节；
  3. **置换表**：以棋盘位棋盘值为键（天然完美哈希），直映射 + 深度校验。
- 启发式权重沿用社区经多年调参验证的经典组合（空格 270 / 合并 700 / 单调性 47 / 大数和 11，幂次 4.0 与 3.5），逐行预计算成 65536 项浮点表。
- 该方案源自 nneonneo 的经典 2048-ai（StackOverflow「What is the optimal algorithm for the game 2048?」高票答案及其 C++ 实现）的公开思路，本仓库为独立实现的 Rust 版本，并加入了自动基准、确定性回放、并发安全设计与完整测试。

## 安全性与并发设计 / Security & Concurrency

- **无 `unsafe` 代码**、**无第三方 crate**（无供应链风险）、**无网络/文件写入**（仅 stdout/stderr）。
- **竞态安全**：`bench` 采用原子计数分发对局索引（每局恰好被认领一次），每线程独享 `Game` + `Expectimax`（含置换表）等全部可变状态，结果经 `mpsc` 通道汇聚——热路径无锁、无共享可变状态。`tests/concurrency.rs` 以 32 局并发自对局 + 同种子确定性对比做回归验证。
- **确定性**：同种子 + 干净置换表 ⇒ 逐位相同的对局（`tests/quality.rs` 依赖此性质做质量回归门槛）。
- **输入校验**：`bestmove` 对非法输入（数量错误、非 2 的幂、超范围）返回退出码 2 而非 panic。
- 已知近似（有意为之，与经典实现一致）：置换表键不含路径概率；两个 32768 合并时棋盘格 rank 封顶为 15（得分仍按 65536 计），该局面实际不可达。

## CI

GitHub Actions（`.github/workflows/ci.yml`）：`rustfmt --check` → `clippy -D warnings` → `cargo test --release`（含 AI 质量门槛：固定种子必须合成 2048）→ CLI 冒烟测试 → 吞吐门槛（单线程 ≥ 10 万节点/秒）→ 自动对局质量门槛（2/2 局达到 512）。

## License

AGPL-3.0-or-later（见 [LICENSE](LICENSE)）。
