# ai2048 —— Rust 版 2048 AI 服务器

[![CI](https://github.com/masgzy/2048ai-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/masgzy/2048ai-rust/actions/workflows/ci.yml)
[![Build](https://github.com/masgzy/2048ai-rust/actions/workflows/build.yml/badge.svg)](https://github.com/masgzy/2048ai-rust/actions/workflows/build.yml)

单文件 Go 版的完全 Rust 重构：位棋盘 expectimax 引擎 + 零依赖 WebSocket 服务器 + 经典 2048 网页界面。
**零第三方依赖**（纯 `std`），任何能装 Rust 的平台都能编译，包括 Android/Termux 与 arm64。

当前版本 **v2.1.0**，许可证 **AGPL-3.0-or-later**（见[许可证](#许可证)）。
服务器默认**只监听 `127.0.0.1:8080`**（仅本机可访问）；需要局域网访问时须显式配置，见[配置](#配置)。

## 性能与强度

- 吞吐：**35 ~ 70M 节点/秒**（视 CPU 核数；2 核沙盒 v2.1.0 实测 `simulate --games 2` 全程平均 49.1M/s）
- 强度：全自动预算（默认最高速度）下自对弈实测 **8192 达成率 2/2**（130,264 / 159,192 分，平均每局 6062 步），
  4096 常规达成
- 移动思考预算完全自动：按空格数与局面复杂度自适应（开局 ~毫秒级，残局自动加时，实测平均每步 40ms）
- 内存占用：每个搜索线程一张约 1.7MB 的置换表（128K 条 × 13 字节）；对象池复用（最多缓存 32 张，约 55MB），
  搜索期零分配

## 快速开始（桌面）

```bash
cargo build --release          # 需要 Rust 1.82 或更高版本
./target/release/ai2048        # 默认监听 127.0.0.1:8080
# 浏览器打开 http://127.0.0.1:8080/
```

可选性能压榨（本机原生指令集自动向量化）：

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

## 配置

命令行选项（`ai2048 --help`）：

| 选项 | 说明 |
| --- | --- |
| `--config, -c <path>` | 配置文件路径。不指定时查找当前目录的 `config.json`，存在才加载 |
| `--host <addr>` | 监听地址，默认 `127.0.0.1`；`0.0.0.0` 表示所有 IPv4 接口 |
| `--port, -p <n>` | 监听端口，默认 `8080` |
| `--weights, -w <path>` | TD 权重文件，默认 `weights.json`（不存在则用内置启发式） |
| `--version, -V` / `--help, -h` | 显示版本 / 帮助 |

配置文件是 JSON，字段全部可选，只写要改的字段。模板见 [`config.example.json`](config.example.json)：

```json
{
  "host": "127.0.0.1",
  "port": 8080,
  "weights": "weights.json",
  "max_connections": 256,
  "idle_timeout_secs": 300
}
```

| 字段 | 默认值 | 取值范围 | 说明 |
| --- | --- | --- | --- |
| `host` | `"127.0.0.1"` | 主机名或 IP | 监听地址 |
| `port` | `8080` | 1 ~ 65535 | 监听端口 |
| `weights` | `"weights.json"` | 非空字符串 | TD 权重文件路径 |
| `max_connections` | `256` | 4 ~ 100000 | 并发连接上限，超出立即回 `503` |
| `idle_timeout_secs` | `300` | 1 ~ 86400 | WebSocket 连接空闲多少秒后断开 |

规则：

- 优先级：内置默认值 < `config.json` < 命令行参数
- `--config` 显式指定的文件读不到、不是合法 JSON 或字段越界：打印原因，退出码 2
- 默认路径 `./config.json` 仅在存在时加载；加载失败则打印警告并退回默认值
- 未知字段忽略

局域网访问（谨慎）：服务**没有身份认证与传输加密**，暴露后同一网络内的任何人都能让它跑引擎（占用 CPU）。
只在可信网络中这样做，不要直接暴露到公网；确有需要请放在带鉴权的反向代理之后，并把 `max_connections` 调低。

```bash
./target/release/ai2048 --host 0.0.0.0     # 命令行
HOST=0.0.0.0 bash termux.sh                 # Termux
# 或写进 config.json：{"host": "0.0.0.0"}
```

## Termux（Android 手机）部署

> **2026-11 已知坑**：Termux 官方 `rust` 包当前缺少 std 静态库（rlib），
> 本地工程 `cargo build` 会报一串
> `crate `std` required to be available in rlib format, but was not found in this form`
> （std / core / alloc / compiler_builtins / libc …）。
> 这是 Termux 打包缺陷（termux-packages issue #31547），**与本项目代码无关**。

### 方式一（推荐）：直接运行预编译静态二进制，无需任何 Rust 环境

仓库 `prebuilt/` 目录附带 aarch64 纯静态二进制（`aarch64-unknown-linux-musl`，
三个文件各约 0.43 ~ 0.58 MB；Android 5.0+ 的 arm64 手机直接运行，不依赖 Termux 的 Rust）。

```bash
bash termux.sh        # 自动检测 prebuilt/ 并启动，默认只监听手机本机 127.0.0.1:8080
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
- 局域网内电脑访问：`HOST=0.0.0.0 bash termux.sh` 启动（默认只监听本机），`ifconfig wlan0` 查看手机 IP，
  浏览器打开 `http://<手机IP>:8080/`
- 一键脚本：`bash termux.sh`（加 `--build` 强制源码编译；`PORT=9000` 换端口）
- RL 训练也能在手机上跑：`./prebuilt/train-android-aarch64 --games 2000`

> arm64 说明：全部代码为可移植标量 Rust（位运算 + 查表），aarch64 基线自带 NEON，
> LLVM 自动向量化即可利用；预编译二进制按通用 aarch64 目标出包以兼容所有机型，
> 自编译加 `target-cpu=native` 可针对你的 SoC（骁龙/天璣/麒麟）进一步优化。
>
> 预编译文件为 v2.1.0 源码的构建产物，已通过 ELF 结构校验（纯静态、无解释器、aarch64），
> 但构建环境不是 Android 手机，未在真机上运行验证；如遇问题请用方式二自行编译。

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
- 页脚常驻许可证（AGPL-3.0-or-later）与本仓库源码地址。

## WebSocket 协议（`/ws`，JSON 文本帧）

握手就是标准的 RFC 6455 升级请求。用 curl 试一次（不带 `Origin` 的非浏览器客户端被允许）：

```bash
curl -i -N --http1.1 http://127.0.0.1:8080/ws \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=='
# HTTP/1.1 101 Switching Protocols
# Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
```

客户端 → 服务端：

```json
{"type":"hello"}
{"type":"move","id":7,"board":[[2,0,0,2],[0,4,0,0],[0,0,0,0],[2,0,0,0]],"min_ms":0}
```

- `board`：4x4 实际牌值（0=空，2,4,…,32768），必须为 2 的幂，非法输入返回 error 不 panic
- `min_ms`：0 = 全自动思考预算；>0 = 每步最短思考毫秒（上限 5000）
- `max_depth`（可选）：搜索深度上限，钳制在 1 ~ 12，默认 12（正常无需传）
- 客户端发出的帧**必须带掩码**；单条消息（分片累计）上限 64KB，单帧上限 1MB

服务端 → 客户端：

```json
{"type":"hello","cores":8,"weights":"内置启发式","version":"2.1.0","engine":"rust-bitboard-expectimax"}
{"type":"move","id":7,"dir":1,"score":1489264.0,"depth":9,"nodes":3260280,"ms":105,"budget":100,"nps":31050285}
{"type":"error","id":99,"error":"非法方块值 3（必须为 2 的幂且 <= 32768）"}
```

- **方向编码：0=左，1=右，2=上，3=下**（与前端一致）
- `dir` 为 0~3；收到后自行应用移动与随机生成块，再把新盘面发回来求下一步
- 其它错误信息：`缺少 type 字段`、`未知消息类型 X`、`JSON 解析失败: …`、`缺少 board 数组`、
  `board 必须为 4x4`、`board 含非法方块值`；引擎内部异常返回 `引擎内部错误`，连接不断开

## 安全边界

默认只监听回环地址是第一道防线；下面是纵深防御，数值都是源码常量（`src/http.rs`、`src/ws.rs`）。

| 环节 | 限制 | 违反时 |
| --- | --- | --- |
| 请求头 | 首行 + 全部头行总长 ≤ 16KB，最多读 64 行 | 超长回 `431` 并断开 |
| 请求头读取时限 | 总时限 15 秒（绝对截止时间，每隔几秒滴 1 个字节也会被切断） | 到期直接断开，不回响应 |
| 写超时 | 30 秒 | 断开 |
| 方法 | 仅 `GET` | 其它方法回 `405` 并带 `Allow: GET` |
| `/ws` 升级 | 必须带 `Upgrade: websocket`、`Connection: Upgrade`、`Sec-WebSocket-Version: 13`、非空 `Sec-WebSocket-Key` | 否则回 `400` |
| 同源校验 | 见下 | 否则回 `403` |
| WebSocket 帧 | 客户端帧必须带掩码；单帧 ≤ 1MB；控制帧不分片且 ≤ 125 字节 | 违规直接断开 |
| WebSocket 消息 | 分片累计 ≤ 64KB（防止大量小分片绕过单帧上限） | 断开 |
| 并发连接 | 默认 256（`max_connections`），线程退出、panic 或创建失败都会归还额度 | 超出回 `503` 并断开 |
| 空闲连接 | WebSocket 阶段默认 300 秒无数据（`idle_timeout_secs`） | 断开 |
| 引擎异常 | 每步计算包在 `catch_unwind` 里 | 返回 error，连接不断 |
| `move` 请求 | `min_ms` 钳制到 ≤ 5000，`max_depth` 钳制到 1 ~ 12 | — |

同源校验规则（防公网页面 drive-by 驱动本机引擎）：

- 不带 `Origin`（curl、脚本、无头工具等非浏览器客户端）：允许
- `Origin` 的 `主机[:端口]` 与 `Host` 头一致（本服务自己提供的页面，含同域反向代理）：允许
- `Origin` 的主机是 `localhost`、`127.0.0.1` 或 `[::1]`（本机其它端口上的页面）：允许
- 其它一律拒绝，`Origin: null`（file:// 页面、沙箱 iframe）也拒绝

仍然存在的边界：没有鉴权、限流和加密；同源校验不是身份验证，任何非浏览器客户端都可以不带 `Origin`。
一条连接同一时刻只处理一个 `move` 请求，每个请求最多起 4 条搜索线程（每个合法方向一条）。

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

参数错误（未知参数、`--games 0`）会打印用法并以退出码 2 结束；`train --help` 列出全部参数。

诚实预期：TD 线性价值函数需要数万局才能逼近甚至超过手调启发式；
训练期间可用 `simulate` 随时评估（见下）。不训练也完全不影响使用——
默认的 nneonneo 式启发式 + expectimax 已经是 2048 公认最强算法组合。

## 基准工具

```bash
cargo run --release --bin simulate -- --games 4 --seed 99
#   --games N      对局数（默认 10）
#   --min-ms 0     全自动预算（与网页默认一致）
#   --max-depth N  搜索深度上限（默认 12）
#   --ref-depth 5  参考模式：纯递归无 TT 版本，用于交叉验证
#   --weights PATH TD 权重文件
```

输出每局分数/最高块、达成率、全程累计的平均节点/秒与平均每步耗时。

## 项目结构

```text
2048ai-rust/
├── Cargo.toml              # 零依赖；release: LTO + codegen-units=1 + strip；MSRV 1.82
├── config.example.json     # 配置文件模板（复制为 config.json 后按需修改，config.json 不入库）
├── termux.sh               # Android 一键启动（默认用预编译版，--build 源码编译）
├── prebuilt/               # aarch64 纯静态预编译二进制（Termux 直接跑）
├── THIRD_PARTY_NOTICES.md  # 第三方 MIT 许可声明
├── .github/
│   ├── workflows/
│   │   ├── ci.yml          # rustfmt / clippy / MSRV / 测试（Linux + Windows）/ 冒烟
│   │   └── build.yml       # 交叉编译矩阵 + ELF 校验 + 打包 + Release
│   └── scripts/            # check_elf.py / smoke.py / package.py / release_pack.py / android_linker.py
└── src/
    ├── lib.rs
    ├── board.rs          # 位棋盘、65536 项行表、转置、RNG（含朴素实现对照测试）
    ├── eval.rs           # 启发式行表（nneonneo 权重）+ TD 权重加载
    ├── engine.rs         # expectimax、TT 对象池、迭代加深、方向级并行、自动预算
    ├── json.rs           # 极简 JSON 解析/转义
    ├── ws.rs             # RFC6455 + SHA1 + base64（含 RFC 向量测试）
    ├── http.rs           # 极简 HTTP 服务 + 路由 + 限制与超时
    ├── ui.rs             # 内嵌前端
    ├── index.html        # 经典 2048 界面（单文件，无外部资源）
    ├── main.rs           # 服务器入口：配置文件 + 命令行
    └── bin/
        ├── train.rs      # TD 强化学习训练器
        └── simulate.rs   # 无头自对弈基准
```

## 持续集成与构建（GitHub Actions）

**CI（`ci.yml`）**：推送到 `main`、推送 `v*` 标签、Pull Request 或手动触发时运行。

- `rustfmt`：`cargo fmt --all --check`
- `clippy`：`cargo clippy --all-targets --locked -- -D warnings`
- `MSRV 1.82`：`cargo +1.82 check --all-targets --locked`
- `测试 (ubuntu-24.04)` 与 `测试 (windows-latest)`：debug 与 release 各跑一遍 `cargo test`，
  构建 release 二进制后让 `simulate` 走一局（`--games 1 --max-depth 3`）、`train` 训练 2 局，
  再用 `.github/scripts/smoke.py` 对真实服务器做端到端冒烟：默认端口与回环监听、配置文件与
  `--port` 覆盖、HTTP 路由、WebSocket 握手与同源校验、未掩码帧、超长请求头、被拒绝的握手等

**Build（`build.yml`）**：每个目标编译 → `check_elf.py` 校验产物确实是目标体系结构（musl 必须纯静态；
Android 必须是带 `/system/bin/linker` 的 PIE 且段对齐 ≥ 16KB，否则 Android 15+ 的 16KB 页设备无法加载）
→ 能在 runner 上运行的目标再做冒烟 → 打包（三个程序 + README + LICENSE + 包内 `SHA256SUMS`）→ 上传产物（保留 14 天）。

已启用 6 个目标（主流使用与测试）：

| 名称 | 目标三元组 | Runner | 产物形态 |
| --- | --- | --- | --- |
| `android_arm64` | `aarch64-linux-android` | ubuntu-24.04 + Android NDK | 动态链接（bionic），16KB 页对齐 |
| `android_armv7` | `armv7-linux-androideabi` | ubuntu-24.04 + Android NDK | 同上 |
| `android_x86` | `i686-linux-android` | ubuntu-24.04 + Android NDK | 同上 |
| `android_x86-64` | `x86_64-linux-android` | ubuntu-24.04 + Android NDK | 同上 |
| `linux_amd64` | `x86_64-unknown-linux-musl` | ubuntu-24.04 | 纯静态 ELF（runner 上冒烟） |
| `windows_amd64` | `x86_64-pc-windows-msvc` | windows-latest | `.exe`（runner 上冒烟） |

预留 13 个目标，写在 `build.yml` 矩阵的注释里，去掉行首的 `# ` 即启用（为避免占用过多公共 runner 资源，平时不构建）：

| 名称 | 目标三元组 | 状态 |
| --- | --- | --- |
| `darwin_amd64` | `x86_64-apple-darwin` | 占位，未在 CI 中验证 |
| `darwin_arm64` | `aarch64-apple-darwin` | 占位，未在 CI 中验证 |
| `linux_386` | `i686-unknown-linux-musl` | 已用 stable 工具链交叉验证可链接出纯静态 ELF（未在真机执行） |
| `linux_arm64` | `aarch64-unknown-linux-musl` | 同上 |
| `linux_armv5` | `armv5te-unknown-linux-musleabi` | 同上（为此引擎已去掉对 64 位原子操作的依赖） |
| `linux_armv6` | `arm-unknown-linux-musleabihf` | 同上 |
| `linux_armv7` | `armv7-unknown-linux-musleabihf` | 同上 |
| `linux_mips` | `mips-unknown-linux-musl` | 占位：stable 不提供预编译标准库，需 nightly + `-Z build-std` 与 MIPS 交叉工具链，未验证 |
| `linux_mips64` | `mips64-unknown-linux-muslabi64` | 同上 |
| `linux_mips64le` | `mips64el-unknown-linux-muslabi64` | 同上 |
| `linux_mipsle` | `mipsel-unknown-linux-musl` | 同上 |
| `windows_386` | `i686-pc-windows-msvc` | 占位，未在 CI 中验证 |
| `windows_arm64` | `aarch64-pc-windows-msvc` | 占位，未在 CI 中验证 |

**发布**：推送形如 `v2.1.0` 的标签（必须等于 `v` + `Cargo.toml` 的 `version`，否则中止）后，
全部目标构建成功、且同一提交的 CI 已通过，才自动创建 GitHub Release：
`ai2048-<版本>-<名称>.tar.gz`（Windows 为 `.zip`）与汇总的 `SHA256SUMS`。
解包后用包内的 `SHA256SUMS` 校验：`sha256sum -c SHA256SUMS`。

```bash
git tag v2.2.0 && git push origin v2.2.0     # 版本号以 Cargo.toml 为准
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

## v2.1.0 变更

安全加固（v2.0.1 的实际行为 → 现在）：

1. 默认监听 `0.0.0.0` → **`127.0.0.1:8080`**，并支持 `config.json`（见[配置](#配置)）。
2. WebSocket 升级不做同源校验，任意公网页面都能驱动本机引擎 → 增加 `Origin` 校验（见[安全边界](#安全边界)）。
3. 不校验 `Sec-WebSocket-Version`、不要求 `Connection: Upgrade`，`/ws` 不带升级头时回 `404` →
   分别回 `400` 并说明原因。
4. 客户端未掩码的帧照常处理 → 按 RFC 6455 直接断开；补齐控制帧与分片累计长度校验。
5. 请求头没有总长上限、也没有总时限（慢速滴字节可长期占用线程）→ 16KB / 64 行 / 15 秒总时限。
6. 没有并发连接上限，线程创建失败时额度不归还 → 默认 256，RAII 归还；写超时 30 秒；
   WebSocket 阶段空闲超时可配。
7. 非 `GET` 方法回 `404` → `405` + `Allow: GET`；读超时后的残缺请求不再被当作完整请求处理。

缺陷修复：

1. `simulate` 的汇总统计（平均节点/秒、平均每步耗时、平均步数）误用“最后一局”的值 → 改为全部对局累计。
2. TT 对象池的锁一旦中毒（某次引擎线程 panic），之后所有请求永久失败 → 从中毒锁中恢复。
3. `simulate` / `train` 遇到未知参数只打印一行就继续运行、退出码为 0 → 打印用法并退出码 2；
   `--games 0` 同样报错；两者补上 `--version` / `--help`。
4. `simulate --max-depth` 的回退值是 8，与文档和默认值 12 不一致 → 统一为 12。
5. `Cargo.toml` 的 `license` 写的是 MIT，而仓库 `LICENSE` 是 AGPL v3 → 统一为 `AGPL-3.0-or-later`，
   页脚常驻许可证与源码地址，附 `THIRD_PARTY_NOTICES.md`。

优化与工程：

1. `count_empty`（机会节点热路径）由 16 次循环改为位技巧的 7 条指令，并与朴素实现对拍测试。
2. 去掉搜索中的 `AtomicU64` 累加（节点数随结果一起收集），同时解除对 64 位原子操作的依赖，
   `linux_armv5` 等目标得以编译。
3. release 配置加 `strip = "symbols"`：aarch64 三个二进制缩小 23% ~ 26%。
4. `cargo fmt` 全量格式化，clippy 零警告；单元测试 38 项（库）+ 5 项（服务器配置），
   新增同源校验、连接上限、慢速请求头、WebSocket 阶段不受请求头时限影响等回归测试。

## 许可证

本程序以 **GNU Affero General Public License v3.0 或更新版本**（`AGPL-3.0-or-later`）发布，
完整条款见 [LICENSE](LICENSE)。你可以自由使用、修改和分发，但分发修改版（包括通过网络
提供服务的修改版）时，必须以相同许可证向用户提供完整的对应源码——页面页脚常驻源码地址即为此意。

本项目参考的两个第三方作品各自以 MIT 许可证发布，著作权归原作者，声明全文见
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)：

- 算法与启发式权重：nneonneo/2048-AI（位棋盘 + expectimax + 启发式行表）
- 界面视觉规范：Gabriele Cirulli 原版 2048

## 致谢

- 算法：nneonneo/2048-AI（位棋盘 + expectimax + 启发式行表）
- 界面视觉规范：Gabriele Cirulli 原版 2048（MIT）
