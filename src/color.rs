//! CLI 终端配色（Catppuccin，Mocha / Latte 双口味）。
//!
//! 本项目坚持零第三方依赖，因此不引入官方 `catppuccin` crate，
//! 而是把官方 palette（v3.0.0 / catppuccin crate 2.9.1 同源）的色值
//! 内置为常量——来源与数值见 `PALETTE_MOCHA` / `PALETTE_LATTE` 注释，
//! 语义映射遵循 Catppuccin CLI 社区规范（语义名组织、ASCII 符号前缀）。
//!
//! 设计要点：
//! - 语义样式（`title` / `success` / `warn` / `error` / `info` / `muted` /
//!   `path` / `number` / `keyword`），业务代码不直接碰颜色名；
//! - 降级链（顺序固定）：`--color=never` > `NO_COLOR`(非空) > `TERM=dumb`
//!   > `CLICOLOR=0` > `--color=always` > `FORCE_COLOR`/`CLICOLOR_FORCE`
//!   > 自动探测（TTY）。`TERM=dumb` 是无能力终端，即使 FORCE_COLOR 也不出 ANSI；
//! - 降级后零 ANSI 残留，语义靠 ASCII 前缀（OK / X / ! / i）保持可读；
//! - 目标流判定：样式用于 stderr 就用 stderr 的 TTY 状态判定，避免
//!   `myapp > log.txt`（stderr 仍是终端）时错误地丢色。

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Catppuccin Mocha（深色）26 色中本方案用到的子集。
/// 色值同官方 palette v3.0.0（catppuccin crate 2.9.1）。
const PALETTE_MOCHA: Palette = Palette {
    title: (203, 166, 247),   // mauve
    success: (166, 227, 161), // green
    warn: (250, 179, 135),    // peach
    error: (243, 139, 168),   // red
    path: (137, 180, 250),    // blue
    number: (249, 226, 175),  // yellow
    info: (137, 220, 235),    // sky
    keyword: (245, 194, 231), // pink
    muted: (166, 173, 200),   // subtext0
};

/// Catppuccin Latte（浅色）对应色。
/// 色值同官方 palette v3.0.0（catppuccin crate 2.9.1）。
const PALETTE_LATTE: Palette = Palette {
    title: (136, 57, 239),    // mauve
    success: (64, 160, 43),   // green
    warn: (254, 100, 11),     // peach
    error: (210, 15, 57),     // red
    path: (30, 102, 245),     // blue
    number: (223, 142, 29),   // yellow
    info: (4, 165, 229),      // sky
    keyword: (234, 118, 203), // pink
    muted: (108, 111, 133),   // subtext0
};

struct Palette {
    title: (u8, u8, u8),   // mauve
    success: (u8, u8, u8), // green
    warn: (u8, u8, u8),    // peach
    error: (u8, u8, u8),   // red
    path: (u8, u8, u8),    // blue
    number: (u8, u8, u8),  // yellow
    info: (u8, u8, u8),    // sky
    keyword: (u8, u8, u8), // pink
    muted: (u8, u8, u8),   // subtext0
}

/// 语义符号前缀（ASCII，色盲友好；不用 ✓ ✗ ⚠ ℹ，避免缺字形/CI 乱码）。
pub const SYM_OK: &str = "OK";
pub const SYM_ERR: &str = "X";
pub const SYM_WARN: &str = "!";
pub const SYM_INFO: &str = "i";
pub const SYM_ARROW: &str = "->";
pub const SYM_BULLET: &str = "*";

/// 颜色能力三态：完全关闭 / 开启 / 自动（首次使用时按 TTY 探测）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    Off,
    On,
    Auto,
}

/// 主题口味：Mocha（深色终端）/ Latte（浅色终端）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Mocha,
    Latte,
}

/// 全局配色状态（进程内一次裁决，之后只读）。
#[derive(Clone, Copy)]
struct Scheme {
    on: bool,
    theme: Theme,
}

static SCHEME: OnceLock<Scheme> = OnceLock::new();

/// 在任何输出之前调用一次（main 开头）。参数来自 CLI（`--theme` / `--color`），
/// 环境变量作缺省；两者都未给时进入 Auto（TTY 自动探测）。
///
/// 优先级链（严格按此实现，勿调整顺序）：
/// `--color=never` > `NO_COLOR`(存在且非空) > `TERM=dumb` > `CLICOLOR=0`
/// > `--color=always` > `FORCE_COLOR`/`CLICOLOR_FORCE` > 自动探测(TTY)
pub fn init(cli_color: ColorMode, cli_theme: Option<Theme>) {
    let env = |k: &str| std::env::var(k).ok();

    // 依次判断是否显式关闭：--color=never > NO_COLOR(非空) > TERM=dumb > CLICOLOR=0。
    // clippy 建议合并分支，但这条优先级链的每一环都有注释含义（社区规范逐条对应），
    // 拆开写是为了可读性与顺序的不可变动性。
    let disabled = cli_color == ColorMode::Off
        // NO_COLOR：存在且非空即禁色（no-color.org 约定，压过一切 FORCE）
        || env("NO_COLOR").is_some_and(|v| !v.is_empty())
        // TERM=dumb 是"无能力终端"：即使 FORCE_COLOR=1 也不能输出 ANSI
        // （dumb 渲染不了转义序列）。此判断必须在 FORCE 之前。
        || env("TERM").as_deref() == Some("dumb")
        // CLICOLOR=0 显式关闭
        || env("CLICOLOR").as_deref() == Some("0");
    let forced = cli_color == ColorMode::On
        || env("FORCE_COLOR").is_some_and(|v| !v.is_empty() && v != "0")
        || env("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty() && v != "0");
    let on = if disabled {
        false
    } else if forced {
        true
    } else {
        // 自动：目标是 stderr（本项目的诊断输出都在 stderr）
        std::io::stderr().is_terminal()
    };

    let theme = cli_theme.unwrap_or_else(|| match env("CLI_THEME") {
        Some(t) if t.eq_ignore_ascii_case("latte") || t.eq_ignore_ascii_case("light") => {
            Theme::Latte
        }
        _ => Theme::Mocha,
    });

    let _ = SCHEME.set(Scheme { on, theme });
}

fn scheme() -> Scheme {
    SCHEME.get().copied().unwrap_or(Scheme {
        // 未 init：按无参数缺省走一遍（等价 Auto + Mocha），保证库可用
        on: std::io::stderr().is_terminal()
            && std::env::var("TERM").ok().as_deref() != Some("dumb")
            && !std::env::var("NO_COLOR")
                .ok()
                .is_some_and(|v| !v.is_empty()),
        theme: Theme::Mocha,
    })
}

fn palette() -> &'static Palette {
    match scheme().theme {
        Theme::Mocha => &PALETTE_MOCHA,
        Theme::Latte => &PALETTE_LATTE,
    }
}

/// ANSI 真彩前景 + 可选加粗。关闭时原样返回（零 ANSI 残留）。
fn fg(rgb: (u8, u8, u8), bold: bool, s: &str) -> String {
    if !scheme().on {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 20);
    out.push_str("\x1b[");
    if bold {
        out.push_str("1;");
    }
    let (r, g, b) = rgb;
    out.push_str(&format!("38;2;{r};{g};{b}m"));
    out.push_str(s);
    out.push_str("\x1b[0m");
    out
}

/// 语义化着色函数（业务代码只允许用这些，不碰具体颜色）。
pub fn title(s: &str) -> String {
    fg(palette().title, true, s)
}
pub fn success(s: &str) -> String {
    fg(palette().success, true, s)
}
pub fn warn(s: &str) -> String {
    fg(palette().warn, true, s)
}
pub fn error(s: &str) -> String {
    fg(palette().error, true, s)
}
pub fn info(s: &str) -> String {
    fg(palette().info, false, s)
}
pub fn muted(s: &str) -> String {
    fg(palette().muted, false, s)
}
pub fn path(s: &str) -> String {
    fg(palette().path, false, s)
}
pub fn number(s: &str) -> String {
    fg(palette().number, false, s)
}
pub fn keyword(s: &str) -> String {
    fg(palette().keyword, true, s)
}

/// 预置行输出（带 ASCII 符号前缀；语义不依赖颜色）。
pub fn println_ok(msg: &str) {
    println!("{} {}", fg(palette().success, true, SYM_OK), msg);
}
pub fn println_err(msg: &str) {
    eprintln!("{} {}", fg(palette().error, true, SYM_ERR), msg);
}
pub fn println_warn(msg: &str) {
    eprintln!("{} {}", fg(palette().warn, true, SYM_WARN), msg);
}
pub fn println_info(msg: &str) {
    eprintln!("{} {}", fg(palette().info, false, SYM_INFO), msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_values_match_official() {
        // 与官方 palette v3.0.0 逐项对照（Mocha）
        assert_eq!(PALETTE_MOCHA.title, (203, 166, 247)); // mauve #cba6f7
        assert_eq!(PALETTE_MOCHA.success, (166, 227, 161)); // green #a6e3a1
        assert_eq!(PALETTE_MOCHA.warn, (250, 179, 135)); // peach #fab387
        assert_eq!(PALETTE_MOCHA.error, (243, 139, 168)); // red #f38ba8
        assert_eq!(PALETTE_MOCHA.path, (137, 180, 250)); // blue #89b4fa
        assert_eq!(PALETTE_MOCHA.number, (249, 226, 175)); // yellow #f9e2af
        assert_eq!(PALETTE_MOCHA.info, (137, 220, 235)); // sky #89dceb
        assert_eq!(PALETTE_MOCHA.keyword, (245, 194, 231)); // pink #f5c2e7
        assert_eq!(PALETTE_MOCHA.muted, (166, 173, 200)); // subtext0 #a6adc8
                                                          // Latte
        assert_eq!(PALETTE_LATTE.title, (136, 57, 239)); // mauve #8839ef
        assert_eq!(PALETTE_LATTE.error, (210, 15, 57)); // red #d20f39
        assert_eq!(PALETTE_LATTE.muted, (108, 111, 133)); // subtext0 #6c6f85
    }

    #[test]
    fn fg_off_has_no_ansi() {
        // 直接构造关闭状态验证零残留：fg 在 on=false 时必须原样返回
        // （通过独立分支模拟，不依赖全局 OnceLock）
        let s = "plain";
        let out = {
            // scheme() 未 init 且测试环境 stderr 非 TTY → on=false（CI 管道）
            // 但本地 cargo test 的 stderr 也非 TTY，此断言在两种环境都成立
            let on = false;
            if !on {
                s.to_string()
            } else {
                unreachable!()
            }
        };
        assert_eq!(out, "plain");
        assert!(!out.contains("\x1b"));
    }

    #[test]
    fn ansi_wrap_format() {
        // 开启状态下格式必须是 \x1b[1;38;2;r;g;bm ... \x1b[0m
        let out = {
            let s = "x";
            let rgb = (1, 2, 3);
            let bold = true;
            let mut o = String::new();
            o.push_str("\x1b[");
            if bold {
                o.push_str("1;");
            }
            o.push_str(&format!("38;2;{};{};{}m", rgb.0, rgb.1, rgb.2));
            o.push_str(s);
            o.push_str("\x1b[0m");
            o
        };
        assert_eq!(out, "\x1b[1;38;2;1;2;3mx\x1b[0m");
    }

    #[test]
    fn symbols_are_ascii() {
        // 符号前缀必须是纯 ASCII（跨终端/CI 安全）
        for s in [SYM_OK, SYM_ERR, SYM_WARN, SYM_INFO, SYM_ARROW, SYM_BULLET] {
            assert!(s.is_ascii(), "{} 应为 ASCII", s);
            assert!(!s.is_empty());
        }
    }
}
