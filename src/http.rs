//! 极简 HTTP 服务器：`/` 返回内嵌页面，`/ws` 升级为 WebSocket。
//! 每连接一个线程；引擎计算在每请求的 scope 线程中进行。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::board::{self, MoveTables};
use crate::engine::{self, TTPool};
use crate::eval::Eval;
use crate::json::Json;
use crate::ui::INDEX_HTML;
use crate::ws;

pub struct App {
    pub tables: Arc<MoveTables>,
    pub eval: Arc<Eval>,
    pub pool: Arc<TTPool>,
    pub cores: usize,
    pub weights_label: String,
    pub version: &'static str,
}

pub fn serve(host: &str, port: u16, app: Arc<App>) -> std::io::Result<()> {
    let listener = TcpListener::bind((host, port))?;
    eprintln!(
        "[ai2048] 监听 http://{}:{}/  （引擎 {} · 评估: {} · CPU {} 核）",
        host, port, app.version, app.weights_label, app.cores
    );
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(s, app);
                });
            }
            Err(e) => eprintln!("[ai2048] accept 失败: {}", e),
        }
    }
    Ok(())
}

struct Req {
    method: String,
    path: String,
    upgrade: bool,
    ws_key: Option<String>,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Req> {
    let mut first = String::new();
    if reader.read_line(&mut first).ok()? == 0 {
        return None;
    }
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let (mut upgrade, mut ws_key) = (false, None);
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end().to_string();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            if k == "upgrade" && v.eq_ignore_ascii_case("websocket") {
                upgrade = true;
            }
            if k == "sec-websocket-key" {
                ws_key = Some(v);
            }
        }
    }
    Some(Req { method, path, upgrade, ws_key })
}

fn handle_conn(stream: TcpStream, app: Arc<App>) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let req = match read_request(&mut reader) {
        Some(r) => r,
        None => return Ok(()),
    };

    if req.method == "GET" && req.path == "/ws" && req.upgrade {
        let key = match req.ws_key {
            Some(k) if !k.is_empty() => k,
            _ => {
                let _ = writer.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
                return Ok(());
            }
        };
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            ws::accept_key(&key)
        );
        writer.write_all(resp.as_bytes())?;
        writer.flush()?;
        // WebSocket 阶段取消读超时
        writer.set_read_timeout(None).ok();
        let mut w = writer;
        ws_session(&mut reader, &mut w, app);
        Ok(())
    } else if req.method == "GET" && (req.path == "/" || req.path == "/index.html") {
        let body = INDEX_HTML;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        );
        writer.write_all(resp.as_bytes())?;
        writer.write_all(body.as_bytes())?;
        writer.flush()
    } else {
        let body = "404 Not Found";
        let resp = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        writer.write_all(resp.as_bytes())?;
        writer.flush()
    }
}

fn ws_session(reader: &mut BufReader<TcpStream>, w: &mut TcpStream, app: Arc<App>) {
    loop {
        let msg = match ws::read_message(reader, w) {
            Some(m) => m,
            None => return,
        };
        if msg.len() > 65536 {
            continue;
        }
        let reply = handle_msg(&msg, &app);
        if ws::write_text(w, &reply).is_err() {
            return;
        }
    }
}

fn err_json(id: Option<u64>, msg: &str) -> String {
    let id = id.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string());
    format!(
        "{{\"type\":\"error\",\"id\":{},\"error\":\"{}\"}}",
        id,
        crate::json::escape(msg)
    )
}

fn handle_msg(data: &[u8], app: &App) -> String {
    let text = String::from_utf8_lossy(data);
    let j = match Json::parse(&text) {
        Ok(j) => j,
        Err(e) => return err_json(None, &format!("JSON 解析失败: {}", e)),
    };
    match j.get("type").and_then(|v| v.as_str()) {
        Some("hello") => format!(
            "{{\"type\":\"hello\",\"cores\":{},\"weights\":\"{}\",\"version\":\"{}\",\"engine\":\"rust-bitboard-expectimax\"}}",
            app.cores,
            crate::json::escape(&app.weights_label),
            app.version
        ),
        Some("move") => handle_move(&j, app),
        Some(other) => err_json(None, &format!("未知消息类型 {}", other)),
        None => err_json(None, "缺少 type 字段"),
    }
}

fn handle_move(j: &Json, app: &App) -> String {
    let id = j.get("id").and_then(|v| v.as_u64());
    let idv = id.unwrap_or(0);

    let boardv = match j.get("board").and_then(|v| v.as_arr()) {
        Some(a) => a,
        None => return err_json(id, "缺少 board 数组"),
    };
    if boardv.len() != 4 {
        return err_json(id, "board 必须为 4x4");
    }
    let mut arr = [[0u32; 4]; 4];
    for r in 0..4 {
        let row = match boardv[r].as_arr() {
            Some(x) => x,
            None => return err_json(id, "board 必须为 4x4"),
        };
        if row.len() != 4 {
            return err_json(id, "board 必须为 4x4");
        }
        for c in 0..4 {
            match row[c].as_u64() {
                Some(v) if v <= 32768 => arr[r][c] = v as u32,
                _ => return err_json(id, "board 含非法方块值"),
            }
        }
    }
    let b = match board::from_array(&arr) {
        Ok(b) => b,
        Err(e) => return err_json(id, &e),
    };
    let min_ms = j.get("min_ms").and_then(|v| v.as_u64()).unwrap_or(0).min(5000);
    let max_depth = j
        .get("max_depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(12)
        .clamp(1, 12) as u8;

    // 引擎 panic 不应拖垮连接线程
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine::choose_move(&app.pool, &app.tables, &app.eval, b, min_ms, max_depth)
    }));

    match result {
        Ok(Ok(r)) => {
            let nps = if r.ms > 0 { r.nodes * 1000 / r.ms.max(1) } else { r.nodes };
            format!(
                "{{\"type\":\"move\",\"id\":{},\"dir\":{},\"score\":{:.1},\"depth\":{},\"nodes\":{},\"ms\":{},\"budget\":{},\"nps\":{}}}",
                idv, r.dir, r.score, r.depth, r.nodes, r.ms, r.budget_ms, nps
            )
        }
        Ok(Err(e)) => err_json(id, &e),
        Err(_) => err_json(id, "引擎内部错误"),
    }
}
