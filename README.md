# ai2048 —— Rust 版 2048 AI 服务器

单文件 Go 版的完全 Rust 重构：位棋盘 expectimax 引擎 + 零依赖 WebSocket 服务器 + 经典 2048 网页界面。
**零第三方依赖**（纯 `std`），任何能装 Rust 的平台都能编译，包括 Android/Termux 与 arm64。

## 性能与强度

- 吞吐：**35 ~ 70M 节点/秒**（视 CPU 核数；沙盒 2 核实测 41~67M/s）
- 强度：全自动预算（默认最高速度）下自对弈实测 **8192 达成率 2/2**（132,988 / 175,644 分），
  4096 常规达成
- 移动思考预算完全自动：按空格数与局面复杂度自适应（开局 ~毫秒级，残局自动加时）
- 内存占用：每线程 2MB 置换表（对象池复用，搜索期零分配）

## 快速开始（桌面）

```bash
cargo build --release
./target/release/ai2048 --port 8080
# 浏览器打开 http://127.0.0.1:8080/
```

可选性能压榨（本机原生指令集自动向量化）：

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

## Termux（Android 手机）部署

> **2026-11 已知坑**：Termux 官方 `rust` 包当前缺少 std 静态库（rlib），
> 本地工程 `cargo build` 会报一串
> `crate `std` required to be available in rlib format, but was not found in this form`
> （std / core / alloc / compiler_builtins / libc …）。
> 这是 Termux 打包缺陷（termux-packages issue #31547），**与本项目代码无关**。

### 方式一（推荐）：直接运行预编译静态二进制，无需任何 Rust 环境

压缩包内 `prebuilt/` 已附带 aarch64 纯静态二进制（musl libc、约 0.7MB，
Android 5.0+ 任意 arm64 手机直接运行，不依赖 Termux 的 Rust）。

```bash
bash termux.sh        # 自动检测 prebuilt/ 并启动
```

手动等价操作：

```bash
chmod +x prebuilt/ai2048-android-aarch64
termux-wake-lock      # 防止息屏后杀后台
./prebuilt/ai2048-android-aarch64 --port 8080
```

### 方式二：源码编译（需要可用的 Rust）

Termux 自带 rust 包有问题时，改用 rustup 工具链即可：

```bash
pkg install -y clang binutils
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
sh /tmp/rustup-init.sh -y --profile minimal
export PATH="$HOME/.cargo/bin:$PATH"   # 关键：别让 Termux 的 rustc 抢在前面
bash termux.sh --build                 # 或手动：
# RUSTFLAGS="-C target-cpu=native" cargo build --release --bin ai2048
```

（Termux 日后修复 rust 包的话，`apt install --reinstall rust` 换回官方包也可以。）

- 手机浏览器打开 `http://127.0.0.1:8080/`
- 局域网内电脑访问：`ifconfig wlan0` 查看手机 IP，浏览器打开 `http://<手机IP>:8080/`
- 一键脚本：`bash termux.sh`（加 `--build` 强制源码编译）
- RL 训练也能在手机上跑：`./prebuilt/train-android-aarch64 --games 2000`

> arm64 说明：全部代码为可移植标量 Rust（位运算 + 查表），aarch64 基线自带 NEON，
> LLVM 自动向量化即可利用；预编译二进制按通用 aarch64 目标出包以兼容所有机型，
> 自编译加 `target-cpu=native` 可针对你的 SoC（骁龙/天璣/麒麟）进一步优化。

## 网页界面（经典 2048 风格）

- 视觉严格复刻原版 2048（Gabriele Cirulli，MIT）的配色与排版，米色背景、经典瓦片色阶、
  分数/最佳/最高块计分板、Game over 遮罩；已做移动端适配与触摸滑动，并显式声明
  `color-scheme: only light` 防止手机浏览器强制暗色反转。
- **自动模式**（默认开启）：AI 连续对局，结束自动开新局重试，8192 达成后自动继续。
- **速度滑杆**：默认 0 = 全自动思考预算（最高速度）；拖大于 0 表示每步至少思考相应毫秒（观看模式）。
  自动预算永远按局面复杂度工作，滑杆只是“最短思考时间”下限，与预算算法无关。
- **并发刷分**：1~8 个独立对局（各自 WebSocket 连接）同时运行并自动重开，
  汇总显示总分/最高单局/8192 次数，主局面与并发局全部计入战绩。
- **引擎数值**：每步的搜索深度、节点数、节点/秒、每步耗时、思考预算、评估分、步数。
- 键盘（方向键 / WASD）或滑动即切回手动模式；分数最佳记录存 localStorage。

## WebSocket 协议（`/ws`，JSON 文本帧）

客户端 → 服务端：

```json
{"type":"hello"}
{"type":"move","id":7,"board":[[2,0,0,2],[0,4,0,0],[0,0,0,0],[2,0,0,0]],"min_ms":0}
```

- `board`：4x4 实际牌值（0=空，2,4,…,32768），必须为 2 的幂，非法输入返回 error 不 panic
- `min_ms`：0 = 全自动思考预算；>0 = 每步最短思考毫秒
- `max_depth`（可选）：搜索深度上限钳制，默认 12（正常无需传）

服务端 → 客户端：

```json
{"type":"hello","cores":8,"weights":"内置启发式","version":"2.0.1","engine":"rust-bitboard-expectimax"}
{"type":"move","id":7,"dir":1,"score":1489264.0,"depth":9,"nodes":3260280,"ms":105,"budget":100,"nps":31050285}
{"type":"error","id":99,"error":"非法方块值 3（必须为 2 的幂且 <= 32768）"}
```

- **方向编码：0=左，1=右，2=上，3=下**（与前端一致）
- `dir` 为 0~3；收到后自行应用移动与随机生成块，再把新盘面发回来求下一步

## 强化学习（TD 训练器）

`train` 是一个真正的自博弈 TD 强化学习器（afterstate TD(0)，线性元组价值函数，
与 Szubert & Jaśkowski 的 2048 TD 方案同族）：

- 价值函数 V(board) = Σ(4 行 + 4 列) 查同一张 65536 项表，与引擎评估接口完全一致
- 每步转移做 δ = (r + max_a' V(s')) − V(afterstate) 更新，ε-greedy 探索
- 产出 `weights.json`，`ai2048` 服务器启动时**自动加载**（页脚显示“评估: TD 学习权重”）

```bash
# 快速体验（弱，仅供验证流程）
cargo run --release --bin train -- --games 500 --alpha 0.001 --eps 0.05
# 认真训练（数小时/数万局，需长时间挂机；每 500 局自动存档）
cargo run --release --bin train -- --games 50000 --alpha 0.0005 --eps 0.02 --out weights.json
# 断点续训
cargo run --release --bin train -- --games 50000 --resume weights.json
```

诚实预期：TD 线性价值函数需要数万局才能逼近甚至超过手调启发式；
训练期间可用 `simulate` 随时评估（见下）。不训练也完全不影响使用——
默认的 nneonneo 式启发式 + expectimax 已经是 2048 公认最强算法组合。

## 基准工具

```bash
cargo run --release --bin simulate -- --games 4 --seed 99
#   --min-ms 0    全自动预算（与网页默认一致）
#   --ref-depth 5 参考模式：纯递归无 TT 版本，用于交叉验证
```

输出每局分数/最高块、达成率、平均节点/秒。

## 项目结构

```
rust2048/
├── Cargo.toml            # 零依赖；release: LTO + codegen-units=1
├── termux.sh             # Android 一键启动（默认用预编译版，--build 源码编译）
├── prebuilt/             # aarch64 纯静态预编译二进制（Termux 直接跑）
└── src/
    ├── lib.rs
    ├── board.rs          # 位棋盘、65536 项行表、转置、RNG（含朴素实现对照测试）
    ├── eval.rs           # 启发式行表（nneonneo 权重）+ TD 权重加载
    ├── engine.rs         # expectimax、TT 对象池、迭代加深、方向级并行、自动预算
    ├── json.rs           # 极简 JSON 解析/转义
    ├── ws.rs             # RFC6455 + SHA1 + base64（含 RFC 向量测试）
    ├── http.rs           # 极简 HTTP 服务 + 路由
    ├── ui.rs             # 内嵌前端
    ├── index.html        # 经典 2048 界面（单文件，无外部资源）
    ├── main.rs           # 服务器入口
    └── bin/
        ├── train.rs      # TD 强化学习训练器
        └── simulate.rs   # 无头自对弈基准
```

## 相比 Go 版修复的 Bug 与优化

1. **致命搜索错误**：机会节点期望值未对空格数取平均，导致“移动后空格多”的方向被
   系统性放大 ×empties，搜索退化为抢空格 → 长期卡在 1024。已对齐 nneonneo 原版
   （`res / num_open`），修复后 8192 稳定达成。
2. 深度上限改为自适应 `distinct_tiles − 2`（原为固定 8，中局浪费、残局不足）。
3. 输入校验：4x4 形状、2 的幂、≤32768 全量校验，非法输入返回 error 而非 panic/越界。
4. 超时污染：deadline 后产生的半成品值不再写入置换表。
5. TT 语义：按“已搜深度 ≥ 所需深度”命中，深结果优先保留（原实现同盘面不同深度互相驱逐）。
6. 分配与并发：TT 对象池复用（搜索期零分配，原版每分支新建 5×65536 切片）；
   去掉 chance 节点 goroutine 风暴与竞态信号量，改为 4 方向各一线程独立迭代加深。
7. 工程：零依赖单二进制、`catch_unwind` 隔离请求 panic、WS 分片/ping/close 全处理。

## 致谢

- 算法：nneonneo/2048-AI（位棋盘 + expectimax + 启发式行表）
- 界面视觉规范：Gabriele Cirulli 原版 2048（MIT）
