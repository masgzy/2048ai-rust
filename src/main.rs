//! ai2048 服务器入口。
//!
//! 配置优先级：内置默认值 < config.json（--config 指定路径，缺省查找示 ./config.json）
//! < 命令行参数。
//!
//! 用法：
//!   ai2048 [--config config.json] [--host 127.0.0.1] [--port 8080] [--weights weights.json]
//!
//! 默认只监听 127.0.0.1:8080（仅本机访问）。局域网访问请显式指定
//! `--host 0.0.0.0` 或在 config.json 中配置，并自行评估暴露风险。

use std::sync::Arc;
use std::time::Duration;

use ai2048::board::MoveTables;
use ai2048::color::{self, ColorMode, Theme};
use ai2048::engine::TTPool;
use ai2048::eval::{self, Eval, Weights};
use ai2048::http::{self, App};
use ai2048::json::Json;

/// 默认配置文件路径（存在才加载）。
const DEFAULT_CONFIG_PATH: &str = "config.json";

/// 服务器配置（config.json，字段全部可选，缺省用内置值）。
struct Config {
    host: String,
    port: u16,
    weights: String,
    max_connections: usize,
    idle_timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // 只监听回环：防止误把引擎暴露到局域网/公网
            host: "127.0.0.1".to_string(),
            port: 8080,
            weights: "weights.json".to_string(),
            max_connections: 256,
            idle_timeout_secs: 300,
        }
    }
}

impl Config {
    fn from_json(j: &Json) -> Result<Config, String> {
        let mut c = Config::default();
        if let Some(h) = j.get("host").and_then(|v| v.as_str()) {
            let h = h.trim();
            if h.is_empty() || h.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
                return Err(format!("host 配置非法: {:?}", h));
            }
            c.host = h.to_string();
        }
        if let Some(p) = j.get("port").and_then(|v| v.as_u64()) {
            c.port = p
                .try_into()
                .map_err(|_| format!("port 必须在 1~65535 之间，当前 {}", p))?;
            if c.port == 0 {
                return Err("port 不允许为 0".to_string());
            }
        }
        if let Some(w) = j.get("weights").and_then(|v| v.as_str()) {
            if w.is_empty() {
                return Err("weights 不能为空字符串".to_string());
            }
            c.weights = w.to_string();
        }
        if let Some(m) = j.get("max_connections").and_then(|v| v.as_u64()) {
            if !(4..=100_000).contains(&m) {
                return Err(format!("max_connections 必须在 4~100000 之间，当前 {}", m));
            }
            c.max_connections = m as usize;
        }
        if let Some(t) = j.get("idle_timeout_secs").and_then(|v| v.as_u64()) {
            if !(1..=86_400).contains(&t) {
                return Err(format!("idle_timeout_secs 必须在 1~86400 之间，当前 {}", t));
            }
            c.idle_timeout_secs = t;
        }
        Ok(c)
    }
}

/// 命令行参数（None = 未指定，不覆盖配置文件）。
struct Cli {
    config_path: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    weights: Option<String>,
    theme: Option<Theme>,
    color: ColorMode,
    help: bool,
    version: bool,
}

fn parse_args() -> Result<Cli, String> {
    let mut cli = Cli {
        config_path: None,
        host: None,
        port: None,
        weights: None,
        theme: None,
        color: ColorMode::Auto,
        help: false,
        version: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("参数 {} 缺少取值", a));
        match a.as_str() {
            "--config" | "-c" => cli.config_path = Some(value()?),
            "--host" => cli.host = Some(value()?),
            "--port" | "-p" => {
                let v = value()?;
                let p: u16 = v.parse().map_err(|_| format!("--port 的值非法: {}", v))?;
                if p == 0 {
                    return Err("--port 不允许为 0".to_string());
                }
                cli.port = Some(p);
            }
            "--weights" | "-w" => cli.weights = Some(value()?),
            "--theme" => {
                let v = value()?;
                cli.theme = Some(match v.as_str() {
                    "mocha" | "dark" => Theme::Mocha,
                    "latte" | "light" => Theme::Latte,
                    other => return Err(format!("--theme 须为 mocha | latte，当前 {}", other)),
                });
            }
            "--color" => {
                let v = value()?;
                cli.color = match v.as_str() {
                    "auto" => ColorMode::Auto,
                    "always" => ColorMode::On,
                    "never" => ColorMode::Off,
                    other => {
                        return Err(format!(
                            "--color 须为 auto | always | never，当前 {}",
                            other
                        ))
                    }
                };
            }
            "--help" | "-h" => cli.help = true,
            "--version" | "-V" => cli.version = true,
            other => return Err(format!("未知参数 {}", other)),
        }
    }
    Ok(cli)
}

fn usage() -> String {
    format!(
        "ai2048 —— 2048 AI 服务器 v{}\n\
         \n\
         用法: ai2048 [选项]\n\
         \n\
         选项:\n\
         \x20 --config <path>    配置文件路径（缺省先查找 ./config.json，存在才加载）\n\
         \x20 --host <addr>      监听地址，默认 127.0.0.1（仅本机；局域网用 0.0.0.0）\n\
         \x20 --port, -p <n>     监听端口，默认 8080\n\
         \x20 --weights, -w <p>  TD 权重文件，默认 weights.json（不存在则用内置启发式）\n\
         \x20 --theme <t>        日志配色口味: mocha | latte（默认 mocha）\n\
         \x20 --color <c>        颜色开关: auto | always | never（亦受 NO_COLOR/TERM 影响）\n\
         \x20 --help, -h         显示本帮助\n\
         \x20 --version, -V      显示版本\n\
         \n\
         配置优先级: 内置默认值 < config.json < 命令行参数\n\
         配置文件字段: host / port / weights / max_connections / idle_timeout_secs\n\
         （参考 config.example.json）",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() {
    let cli = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ai2048] 参数错误: {}（--help 查看用法）", e);
            std::process::exit(2);
        }
    };
    if cli.help {
        println!("{}", usage());
        return;
    }
    if cli.version {
        println!("ai2048 {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // 配色：在任何日志输出之前裁决（NO_COLOR / TERM=dumb / 非 TTY 自动降级）
    color::init(cli.color, cli.theme);

    // 配置文件：显式 --config 必须加载成功；默认路径存在才尝试
    let mut cfg = Config::default();
    let path = cli
        .config_path
        .clone()
        .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
    let explicit = cli.config_path.is_some();
    if explicit || std::path::Path::new(&path).exists() {
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| Json::parse(&text).map_err(|e| e.to_string()))
            .and_then(|j| Config::from_json(&j))
        {
            Ok(c) => {
                eprintln!("[ai2048] 已加载配置文件: {}", path);
                cfg = c;
            }
            Err(e) if explicit => {
                eprintln!("[ai2048] 配置文件 {} 加载失败: {}", path, e);
                std::process::exit(2);
            }
            Err(e) => {
                eprintln!("[ai2048] 配置文件 {} 加载失败: {}（改用默认配置）", path, e);
            }
        }
    }
    // 命令行参数覆盖配置文件
    if let Some(h) = cli.host {
        cfg.host = h;
    }
    if let Some(p) = cli.port {
        cfg.port = p;
    }
    if let Some(w) = cli.weights {
        cfg.weights = w;
    }

    let tables = Arc::new(MoveTables::new());

    let (ev, label) = match eval::load_file(&cfg.weights) {
        Ok(e) => {
            color::println_info(&format!("已加载 TD 学习权重: {}", cfg.weights));
            (Arc::new(e), format!("TD 学习权重({})", cfg.weights))
        }
        Err(e) => {
            if std::path::Path::new(&cfg.weights).exists() {
                color::println_warn(&format!(
                    "加载 {} 失败: {}（改用内置启发式）",
                    cfg.weights, e
                ));
            }
            (
                Arc::new(Eval::build(&Weights::default())),
                "内置启发式".to_string(),
            )
        }
    };

    let pool = Arc::new(TTPool::new());
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let app = Arc::new(App {
        tables,
        eval: ev,
        pool,
        cores,
        weights_label: label,
        version: env!("CARGO_PKG_VERSION"),
        max_conns: cfg.max_connections,
        idle_timeout: Duration::from_secs(cfg.idle_timeout_secs),
    });

    println!(
        "{}",
        color::title("[ai2048] 位棋盘 expectimax 引擎就绪（Rust，零第三方依赖）")
    );
    if cfg.host == "127.0.0.1" || cfg.host == "localhost" || cfg.host == "::1" {
        color::println_info("仅监听本机回环；局域网访问需 --host 0.0.0.0（注意暴露风险）");
    }
    if let Err(e) = http::serve(&cfg.host, cfg.port, app) {
        color::println_err(&format!("服务器启动失败: {}", e));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_when_empty() {
        let j = Json::parse("{}").unwrap();
        let c = Config::from_json(&j).unwrap();
        assert_eq!((c.host.as_str(), c.port), ("127.0.0.1", 8080));
        assert_eq!(c.weights, "weights.json");
        assert_eq!(c.max_connections, 256);
        assert_eq!(c.idle_timeout_secs, 300);
    }

    #[test]
    fn config_overrides() {
        let j = Json::parse(
            r#"{"host":"0.0.0.0","port":9000,"weights":"w.json","max_connections":64,"idle_timeout_secs":30}"#,
        )
        .unwrap();
        let c = Config::from_json(&j).unwrap();
        assert_eq!(c.host, "0.0.0.0");
        assert_eq!(c.port, 9000);
        assert_eq!(c.weights, "w.json");
        assert_eq!(c.max_connections, 64);
        assert_eq!(c.idle_timeout_secs, 30);
    }

    #[test]
    fn config_rejects_bad_values() {
        for bad in [
            r#"{"port":65536}"#,
            r#"{"port":0}"#,
            r#"{"host":""}"#,
            r#"{"host":"bad host"}"#,
            r#"{"host":"127.0.0.1\nx"}"#,
            r#"{"max_connections":1}"#,
            r#"{"max_connections":9999999}"#,
            r#"{"idle_timeout_secs":0}"#,
            r#"{"weights":""}"#,
        ] {
            let j = Json::parse(bad).unwrap();
            assert!(Config::from_json(&j).is_err(), "应拒绝配置: {}", bad);
        }
    }

    #[test]
    fn config_trims_surrounding_whitespace() {
        // 配置值首尾空白应被宽容处理（trim 后使用），而非报错
        let j = Json::parse(r#"{"host":" 127.0.0.1 "}"#).unwrap();
        let c = Config::from_json(&j).unwrap();
        assert_eq!(c.host, "127.0.0.1");
    }

    #[test]
    fn config_ignores_unknown_fields() {
        let j = Json::parse(r#"{"future_option":true,"port":9999}"#).unwrap();
        let c = Config::from_json(&j).unwrap();
        assert_eq!(c.port, 9999);
    }
}
