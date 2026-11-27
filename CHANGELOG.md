# Changelog

本项目的所有显著变更都记录在此文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。
alpha 阶段次版本号表示一次不保证兼容的迭代；正式 1.0.0 起按 semver 常规执行。

## [Unreleased]

### 新增

- **多线程训练**：`train --threads N`。每线程独立网络自博弈（无锁竞争），
  每 `--merge-every` 局（默认 250）全网络平均合并——TDL2048 的 20 线程
  lock-free optimistic 并行的保守变体。2 vCPU 沙盒实测 4 线程吞吐 ×2.25。
- **OTD / OTC / OTD+TC 学习器**（`src/learn.rs`，方法族与 TDL2048+ 同源，
  Guei et al. IEEE TG 2021）：
  - 乐观初始化 `--vinit`（默认 80000，均摊到 8 元组）；
  - TC 学习：逐权重 β = |E|/A 调制有效学习率；
  - OTD+TC 两阶段（默认）：前 90% 局 OTD、后 10% TC 微调（`--ptc`）；
  - `--mode otd | otc | otd-tc` 显式选择。
- **多阶段网络**：`--stages 8192`（默认两阶段）或 `--stages 8192,16384`
  （三阶段）；各阶段独立权重表，缓解大块状态训练样本不足。
  权重文件新增多阶段格式（`tables` 数组），**旧单阶段文件仍可加载**；
  服务器/评估取第一阶段表。
- **Tile-downgrading 搜索**（`simulate --dg`）：根盘面存在 32768+ 大块且
  存在缺失中间块时，把大于最大缺失块的块减半后在降级盘面上搜索
  （TDL2048+ 的 Tile-DG，论文 IV-C.3）。
- **CLI 配色**（`src/color.rs`，Catppuccin Mocha/Latte）：
  - 色值内置自官方 palette v3.0.0（零依赖工程不引 crate）；
  - 语义化样式（title/success/warn/error/info/muted/path/number/keyword）
    + ASCII 符号前缀（OK/X/!/i）；
  - 降级链：`--color=never` > `NO_COLOR`(非空) > `TERM=dumb` > `CLICOLOR=0`
    > `--color=always` > `FORCE_COLOR`/`CLICOLOR_FORCE` > TTY 自动探测；
    `TERM=dumb` 即使 FORCE_COLOR 也不出 ANSI；降级后零 ANSI 残留；
  - `train` / `simulate` / `ai2048` 均支持 `--theme` / `--color`，
    服务器日志同步着色。

### 修复

- Tile-DG 的"最大缺失块"判定：原实现只处理"从最大块往下连续缺失"的情况，
  在中间块缺失（如 256 缺而 512~16384 都在）时会误判为无缺失；
  修正为扫描全部 rank 取最大缺失者（由新增单元测试抓出）。

## [1.0.0-alpha.1] - 2026-11-27

首个发布版。以随包分发的 Go 重构初版（v2.0.1，内部版本号）为基线审查并加固而来，
引擎算法与该版一致，修复与加固如下。

### 安全

- 默认监听 `0.0.0.0` 改为 **`127.0.0.1:8080`**；局域网访问须显式配置（`--host` / `HOST` / `config.json`）。
- WebSocket 升级增加 `Origin` 同源校验：公网页面 drive-by 驱动本机引擎的请求回 `403`，
  `Origin: null` 拒绝；不带 `Origin` 的非浏览器客户端（curl/脚本）仍允许。
- `/ws` 升级请求补全校验：缺 `Upgrade`/`Connection: Upgrade`/`Sec-WebSocket-Version: 13`/
  `Sec-WebSocket-Key` 分别回 `400` 并说明原因（此前要么回 404 要么照常 101）。
- 客户端帧按 RFC 6455 强制掩码，未掩码直接断开；控制帧必须不分片且 ≤ 125 字节。
- 消息长度上限：单帧 ≤ 1MB，分片累计 ≤ 64KB，超限断开。
- 请求头限制：首行 + 头行总长 ≤ 16KB（超出回 `431`）、最多 64 行、**15 秒总时限**
  （绝对截止，逐字节慢滴也无法长期占用线程）。
- 并发连接上限默认 256（`max_connections` 可配，4 ~ 100000），超出回 `503`；
  连接额度由 RAII 守卫归还——线程退出、panic、创建失败都不会泄漏。
- 写超时 30 秒；WebSocket 阶段空闲超时默认 300 秒（`idle_timeout_secs` 可配）。
- 非 `GET` 方法回 `405` + `Allow: GET`（原为 404）；读超时后的残缺请求不再被当作完整请求处理。

### 修复

- **Windows：WebSocket 连接升级后被立即断开**。请求头阶段的绝对总时限会在 socket 上留下
  "剩余期限"的读超时残留（可能只剩几百毫秒），升级后只通过 `try_clone` 复制的句柄设置新超时，
  Windows 上不生效，连接随即被误判空闲断开（客户端表现为 10053 ConnectionAborted）。
  现在把空闲超时直接设在读取句柄上，并给写出句柄补设写超时与 nodelay。（由 CI 的
  Windows runner 首轮运行发现，Linux 因 `dup` 共享 socket 选项而无法复现。）
- `simulate` 汇总统计（平均节点/秒、平均每步耗时、平均步数）误用最后一局的值 → 改为全部对局累计，
  并以 `checked_div` 防除零。
- TT 对象池互斥锁一旦中毒（任一搜索线程 panic），之后所有请求永久失败 → 从中毒锁中恢复
  （置换表只是缓存，不值得全局报废）。
- `simulate` / `train` 未知参数只打印一行就继续运行、退出码 0 → 打印用法并以退出码 2 结束；
  `--games 0` 同样报错；两者补齐 `--version` / `--help`。
- `simulate --max-depth` 回退值 8 与文档及默认值 12 不一致 → 统一为 12。
- `Cargo.toml` 的 `license` 为 MIT 而仓库 `LICENSE` 为 AGPL v3 → 统一为 `AGPL-3.0-or-later`，
  网页页脚常驻许可证与源码地址，附 `THIRD_PARTY_NOTICES.md`。

### 变更

- 配置文件支持：`config.json`（字段全部可选：`host` / `port` / `weights` / `max_connections` /
  `idle_timeout_secs`），优先级 内置默认值 < 配置文件 < 命令行参数；模板见 `config.example.json`。

### 性能

- `count_empty`（机会节点热路径）由 16 次循环改为位技巧（7 条指令），新增与朴素实现的
  2000 盘面对拍测试。
- 去掉搜索中的 `AtomicU64` 原子累加（节点数随结果收集），消除争用写，同时解除对 64 位原子
  操作的依赖，`linux_armv5` 等目标得以编译。
- release 配置 `strip = "symbols"`：aarch64 二进制缩小 23% ~ 26%。

### 工程

- GitHub Actions：CI（rustfmt / clippy -D warnings / MSRV 1.82 / ubuntu+windows 双平台测试 /
  `simulate`/`train` 实跑 / 39 项端到端冒烟）与 Build（6 目标交叉编译 + ELF 结构校验 +
  runner 上冒烟 + 打包上传），推送 `v*` 标签自动创建 Release。
- `cargo fmt` 全量格式化，clippy 零警告；单元测试 43 项（库 38 + 服务器配置 5）。
- 预编译 `prebuilt/` 换为本版构建并通过 ELF 校验（aarch64 musl 纯静态）。

### 相对 Go 重构初版（基线）已包含的算法修复

- 机会节点期望值按空格数取平均（原实现系统性偏向抢空格，长期卡 1024，修复后 8192 稳定达成）。
- 深度上限自适应 `distinct_tiles − 2`；deadline 后的半成品值不写入置换表；
  TT 按"已搜深度 ≥ 所需深度"命中；TT 对象池复用（搜索期零分配）；
  4 方向各一线程独立迭代加深，替代 chance 节点线程风暴。

[Unreleased]: https://github.com/masgzy/2048ai-rust/compare/v1.0.0-alpha.1...HEAD
[1.0.0-alpha.1]: https://github.com/masgzy/2048ai-rust/releases/tag/v1.0.0-alpha.1
