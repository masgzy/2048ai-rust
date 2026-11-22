//! ai2048 服务器入口。
//!
//! 用法：
//!   ai2048 [--port 8080] [--host 0.0.0.0] [--weights weights.json]

use std::sync::Arc;

use ai2048::board::MoveTables;
use ai2048::engine::TTPool;
use ai2048::eval::{self, Eval, Weights};
use ai2048::http::{self, App};

fn main() {
    let mut port: u16 = 8080;
    let mut host = "0.0.0.0".to_string();
    let mut weights_path = String::from("weights.json");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" | "-p" => {
                port = args.next().and_then(|v| v.parse().ok()).unwrap_or(8080);
            }
            "--host" => {
                host = args.next().unwrap_or_else(|| "0.0.0.0".to_string());
            }
            "--weights" | "-w" => {
                weights_path = args.next().unwrap_or_else(|| "weights.json".to_string());
            }
            "--help" | "-h" => {
                println!("用法: ai2048 [--port 8080] [--host 0.0.0.0] [--weights weights.json]");
                return;
            }
            other => {
                eprintln!("未知参数 {}（--help 查看用法）", other);
                return;
            }
        }
    }

    let tables = Arc::new(MoveTables::new());

    let (ev, label) = match eval::load_file(&weights_path) {
        Ok(e) => {
            eprintln!("[ai2048] 已加载 TD 学习权重: {}", weights_path);
            (Arc::new(e), format!("TD 学习权重({})", weights_path))
        }
        Err(e) => {
            if std::path::Path::new(&weights_path).exists() {
                eprintln!("[ai2048] 加载 {} 失败: {}（改用内置启发式）", weights_path, e);
            }
            (Arc::new(Eval::build(&Weights::default())), "内置启发式".to_string())
        }
    };

    let pool = Arc::new(TTPool::new());
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    let app = Arc::new(App {
        tables,
        eval: ev,
        pool,
        cores,
        weights_label: label,
        version: env!("CARGO_PKG_VERSION"),
    });

    eprintln!("[ai2048] 位棋盘 expectimax 引擎就绪（Rust，零第三方依赖）");
    eprintln!("[ai2048] 手机/Termux 上可用浏览器打开 http://127.0.0.1:{}/", port);
    if let Err(e) = http::serve(&host, port, app) {
        eprintln!("[ai2048] 服务器启动失败: {}", e);
        std::process::exit(1);
    }
}
