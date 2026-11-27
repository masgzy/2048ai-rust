//! 极简 HTTP 服务器：`/` 返回内嵌页面，`/ws` 升级为 WebSocket。
//! 每连接一个线程；引擎计算在每请求的 scope 线程中进行。
//!
//! 安全边界（默认仅监听 127.0.0.1，以下为纵深防御）：
//! - 请求头总字节数与行数双重上限（防慢速/超长头内存攻击）；
//! - 并发连接数上限（线程数封顶，防连接洪泛）；
//! - WebSocket 升级做同源校验（防公网页面 drive-by 驱动本机引擎）；
//! - WS 消息总长度在协议层强制（见 `ws::read_message`）；
//! - 请求头读取有 15s **总时限**（绝对截止时间，见 `DeadlineReader`）、
//!   WS 阶段空闲超时可配、写超时 30s（防慢速读写长期占用线程）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::board::{self, MoveTables};
use crate::engine::{self, TTPool};
use crate::eval::Eval;
use crate::json::Json;
use crate::ui::INDEX_HTML;
use crate::ws;

/// 请求头总字节上限（首行 + 全部头行），超出返回 431。
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// 请求头最大行数。
const MAX_HEADER_LINES: usize = 64;
/// 单条 WebSocket 消息（分片累计后）上限。
const MAX_WS_MSG: usize = 64 * 1024;
/// 请求头阶段总时限（绝对截止时间，而非单次读的空闲超时）。
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(15);
/// 写超时（防慢速客户端拖死线程）。
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct App {
    pub tables: Arc<MoveTables>,
    pub eval: Arc<Eval>,
    pub pool: Arc<TTPool>,
    pub cores: usize,
    pub weights_label: String,
    pub version: &'static str,
    /// 最大并发连接数（超出立刻回 503 并断开）。
    pub max_conns: usize,
    /// WebSocket 空闲读超时。
    pub idle_timeout: Duration,
}

pub fn serve(host: &str, port: u16, app: Arc<App>) -> std::io::Result<()> {
    let listener = TcpListener::bind((host, port))?;
    serve_on(listener, app)
}

/// 在既有 listener 上服务（测试用：绑 127.0.0.1:0 拿随机端口）。
pub fn serve_on(listener: TcpListener, app: Arc<App>) -> std::io::Result<()> {
    // 仅作计数用，无跨线程内存序依赖，Relaxed 足够。
    let live: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let s = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ai2048] accept 失败: {}", e);
                continue;
            }
        };
        if live.fetch_add(1, Ordering::Relaxed) >= app.max_conns {
            live.fetch_sub(1, Ordering::Relaxed);
            let _ = write_simple(&s, "503 Service Unavailable", "503 too many connections");
            eprintln!("[ai2048] 连接数达到上限 {}，拒绝新连接", app.max_conns);
            continue;
        }
        let app = app.clone();
        let guard = ConnGuard(live.clone());
        let spawned = std::thread::Builder::new().spawn(move || {
            // guard 在线程结束时析构归还配额；spawn 失败时闭包被丢弃，
            // guard 随之析构——两条路径都不会泄漏连接额度。
            let _guard = guard;
            let _ = handle_conn(s, app);
        });
        if let Err(e) = spawned {
            eprintln!("[ai2048] 线程创建失败: {}", e);
        }
    }
    Ok(())
}

/// RAII 连接计数：线程退出（含 panic）或 spawn 失败时自动归还配额。
struct ConnGuard(Arc<AtomicUsize>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 请求头阶段的只读流：每次底层 read 前把读超时设为"剩余时间"，
/// 使请求头读取受绝对总时限约束。只用单次读超时是不够的——
/// 客户端每隔十几秒滴 1 个字节，就能把一个线程拖住数小时。
/// 进入 WebSocket 阶段后把 `deadline` 置为 None，改由套接字自身的
/// 空闲超时接管。
struct DeadlineReader {
    inner: TcpStream,
    deadline: Option<Instant>,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(d) = self.deadline {
            let rest = d.saturating_duration_since(Instant::now());
            if rest.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request header timeout",
                ));
            }
            self.inner.set_read_timeout(Some(rest))?;
        }
        self.inner.read(buf)
    }
}

fn write_simple(mut w: &TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    w.write_all(resp.as_bytes())?;
    w.flush()
}

struct Req {
    method: String,
    path: String,
    upgrade: bool,
    connection_upgrade: bool,
    ws_key: Option<String>,
    ws_version: Option<String>,
    origin: Option<String>,
    host: Option<String>,
}

/// 读请求。`Ok(None)`：连接在请求开始前关闭 / 读超时；`Err`：请求头超限等格式错误。
fn read_request(reader: &mut BufReader<DeadlineReader>) -> Result<Option<Req>, ()> {
    let mut first = String::new();
    // 读失败或超时（含总时限到期）按"连接已结束"处理，不回错误响应
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return Ok(None);
    }
    let mut total = first.len();
    if total > MAX_HEADER_BYTES {
        return Err(());
    }
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let (mut upgrade, mut connection_upgrade, mut ws_key, mut ws_version, mut origin, mut host) =
        (false, false, None, None, None, None);
    for _ in 0..MAX_HEADER_LINES {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            // 对端在行边界关闭：按请求头结束处理（宽容对待 nc 等简易客户端）
            Ok(0) => break,
            Ok(_) => {}
            // 读超时（含总时限到期）或读错误：残缺的请求不得被当作完整请求处理
            Err(_) => return Ok(None),
        }
        total += line.len();
        if total > MAX_HEADER_BYTES {
            return Err(());
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match k.as_str() {
                "upgrade" if v.eq_ignore_ascii_case("websocket") => upgrade = true,
                // Connection 头可能含多个 token（如 "keep-alive, Upgrade"）
                "connection" => {
                    connection_upgrade = v
                        .split(',')
                        .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
                }
                "sec-websocket-key" => ws_key = Some(v.to_string()),
                "sec-websocket-version" => ws_version = Some(v.to_string()),
                "origin" => origin = Some(v.to_string()),
                "host" => host = Some(v.to_string()),
                _ => {}
            }
        }
    }
    Ok(Some(Req {
        method,
        path,
        upgrade,
        connection_upgrade,
        ws_key,
        ws_version,
        origin,
        host,
    }))
}

/// WebSocket 升级的同源校验：
/// - 无 Origin（非浏览器客户端：curl、脚本、无头工具）：允许；
/// - Origin 的 host[:port] 与 Host 头一致（本服务自己提供的页面，含同域反代）：允许；
/// - Origin 为 localhost / 127.0.0.1 / [::1]（本机其它端口的页面）：允许；
/// - 其余一律拒绝——浏览器允许任意公网页面向 127.0.0.1 发起 WebSocket，
///   不校验会被恶意网页 drive-by 烧 CPU / 白嫖算力。
///
/// `null` 源（file://、被沙箱化的 iframe）也拒绝：沙箱 iframe 可产出 null 源。
fn origin_allowed(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let Some(host) = host.map(str::trim).filter(|h| !h.is_empty()) else {
        return false;
    };
    let authority = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin)
        .split('/')
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    if authority.eq_ignore_ascii_case(host) {
        return true;
    }
    let origin_host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority);
    matches!(
        origin_host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

fn handle_conn(stream: TcpStream, app: Arc<App>) -> std::io::Result<()> {
    handle_conn_with(stream, app, HTTP_READ_TIMEOUT)
}

/// `header_timeout` 为请求头阶段的总时限（测试可缩短）。
fn handle_conn_with(
    stream: TcpStream,
    app: Arc<App>,
    header_timeout: Duration,
) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
    let mut writer = stream.try_clone()?;
    // try_clone 复制的句柄在部分平台上不保证继承 socket 选项：写路径的超时与
    // nodelay 需要对 writer 自己再设一次（读路径同理，见下方升级处）。
    writer.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
    writer.set_nodelay(true).ok();
    let mut reader = BufReader::new(DeadlineReader {
        inner: stream,
        deadline: Some(Instant::now() + header_timeout),
    });

    let req = match read_request(&mut reader) {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(()),
        Err(()) => {
            let _ = write_simple(
                &writer,
                "431 Request Header Fields Too Large",
                "431 header too large",
            );
            return Ok(());
        }
    };

    if req.method != "GET" {
        let body = "405 Method Not Allowed";
        let resp = format!(
            "HTTP/1.1 405 Method Not Allowed\r\nAllow: GET\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        writer.write_all(resp.as_bytes())?;
        return writer.flush();
    }

    if req.path == "/ws" {
        if !(req.upgrade && req.connection_upgrade) {
            let _ = write_simple(&writer, "400 Bad Request", "400 websocket upgrade required");
            return Ok(());
        }
        if req.ws_version.as_deref() != Some("13") {
            let _ = write_simple(
                &writer,
                "400 Bad Request",
                "400 Sec-WebSocket-Version must be 13",
            );
            return Ok(());
        }
        let key = match req.ws_key {
            Some(k) if !k.trim().is_empty() => k,
            _ => {
                let _ = write_simple(&writer, "400 Bad Request", "400 missing Sec-WebSocket-Key");
                return Ok(());
            }
        };
        if !origin_allowed(req.origin.as_deref(), req.host.as_deref()) {
            let _ = write_simple(
                &writer,
                "403 Forbidden",
                "403 cross-origin websocket refused",
            );
            return Ok(());
        }
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            ws::accept_key(&key)
        );
        writer.write_all(resp.as_bytes())?;
        writer.flush()?;
        // WebSocket 阶段：解除请求头总时限，改用可配置的空闲超时。
        // 读超时必须设在 reader 自己持有的那条句柄上：DeadlineReader 在请求头
        // 阶段会把 SO_RCVTIMEO 设成"剩余期限"（可能只剩几十~几百毫秒），
        // 若升级时只通过 writer 的复制句柄去改（Windows CI 实测不生效），
        // reader 会沿用残留的短超时，升级后立刻被当作空闲连接断开。
        reader.get_mut().deadline = None;
        reader
            .get_mut()
            .inner
            .set_read_timeout(Some(app.idle_timeout))
            .ok();
        writer.set_read_timeout(Some(app.idle_timeout)).ok();
        let mut w = writer;
        ws_session(&mut reader, &mut w, &app);
        Ok(())
    } else if req.path == "/" || req.path == "/index.html" {
        let body = INDEX_HTML;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        );
        writer.write_all(resp.as_bytes())?;
        writer.write_all(body.as_bytes())?;
        writer.flush()
    } else {
        let _ = write_simple(&writer, "404 Not Found", "404 Not Found");
        Ok(())
    }
}

fn ws_session(reader: &mut BufReader<DeadlineReader>, w: &mut TcpStream, app: &App) {
    loop {
        // 消息长度上限在协议层强制：超限断开而非跳过
        let msg = match ws::read_message(reader, w, MAX_WS_MSG) {
            Some(m) => m,
            None => return,
        };
        let reply = handle_msg(&msg, app);
        if ws::write_text(w, &reply).is_err() {
            return;
        }
    }
}

fn err_json(id: Option<u64>, msg: &str) -> String {
    let id = id
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string());
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
    for (r, rowv) in boardv.iter().enumerate() {
        let row = match rowv.as_arr() {
            Some(x) => x,
            None => return err_json(id, "board 必须为 4x4"),
        };
        if row.len() != 4 {
            return err_json(id, "board 必须为 4x4");
        }
        for (c, cell) in row.iter().enumerate() {
            match cell.as_u64() {
                Some(v) if v <= 32768 => arr[r][c] = v as u32,
                _ => return err_json(id, "board 含非法方块值"),
            }
        }
    }
    let b = match board::from_array(&arr) {
        Ok(b) => b,
        Err(e) => return err_json(id, &e),
    };
    let min_ms = j
        .get("min_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(5000);
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
            // ms 为 0 时 nps 无意义，退回节点数本身
            let nps = (r.nodes * 1000).checked_div(r.ms).unwrap_or(r.nodes);
            format!(
                "{{\"type\":\"move\",\"id\":{},\"dir\":{},\"score\":{:.1},\"depth\":{},\"nodes\":{},\"ms\":{},\"budget\":{},\"nps\":{}}}",
                idv, r.dir, r.score, r.depth, r.nodes, r.ms, r.budget_ms, nps
            )
        }
        Ok(Err(e)) => err_json(id, &e),
        Err(_) => err_json(id, "引擎内部错误"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Eval, Weights};
    use std::io::Read;
    use std::net::Shutdown;

    fn test_app() -> Arc<App> {
        Arc::new(App {
            tables: Arc::new(MoveTables::new()),
            eval: Arc::new(Eval::build(&Weights::default())),
            pool: Arc::new(TTPool::new()),
            cores: 1,
            weights_label: "内置启发式".to_string(),
            version: "test",
            max_conns: 16,
            idle_timeout: Duration::from_secs(60),
        })
    }

    fn spawn_server() -> std::net::SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let app = test_app();
        std::thread::spawn(move || {
            let _ = serve_on(l, app);
        });
        addr
    }

    fn recv_all(s: &mut TcpStream) -> String {
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        buf
    }

    /// 在已连接的套接字上发送 WS 握手请求，返回响应头。
    fn ws_upgrade(s: &mut TcpStream, origin: Option<&str>, host: &str) -> String {
        let mut req = format!(
            "GET /ws HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n",
            host
        );
        if let Some(o) = origin {
            req.push_str(&format!("Origin: {}\r\n", o));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).unwrap();
        // 读到响应头结束（\r\n\r\n）为止，避免短读
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            match s.read(&mut byte) {
                Ok(0) => break, // 对端已关闭（错误响应可能直接断开）
                Ok(_) => buf.push(byte[0]),
                Err(_) => break,
            }
            if buf.len() > 8192 {
                break;
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// 发起 WS 握手，返回 (流, 响应头)。
    fn ws_handshake(
        addr: std::net::SocketAddr,
        origin: Option<&str>,
        host: &str,
    ) -> (TcpStream, String) {
        let mut s = TcpStream::connect(addr).unwrap();
        let resp = ws_upgrade(&mut s, origin, host);
        (s, resp)
    }

    /// 写一帧客户端样式的掩码文本帧。
    fn ws_send_text(s: &mut TcpStream, text: &str) {
        let payload = text.as_bytes();
        let mask = [0x11, 0x22, 0x33, 0x44];
        let mut frame = vec![0x81];
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        s.write_all(&frame).unwrap();
    }

    /// 读一条服务端文本消息（未掩码帧）。
    fn ws_recv(s: &mut TcpStream) -> String {
        let mut h = [0u8; 2];
        s.read_exact(&mut h).unwrap();
        let mut len = (h[1] & 0x7F) as usize;
        if len == 126 {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).unwrap();
            len = u16::from_be_bytes(b) as usize;
        } else if len == 127 {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).unwrap();
            len = u64::from_be_bytes(b) as usize;
        }
        assert_eq!(h[1] & 0x80, 0, "服务端帧不应掩码");
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload).unwrap();
        String::from_utf8(payload).unwrap()
    }

    #[test]
    fn origin_rules() {
        // 非浏览器客户端（无 Origin）：允许
        assert!(origin_allowed(None, None));
        assert!(origin_allowed(None, Some("127.0.0.1:8080")));
        // 同源（Origin 与 Host 的 authority 一致）
        assert!(origin_allowed(
            Some("http://127.0.0.1:8080"),
            Some("127.0.0.1:8080")
        ));
        assert!(origin_allowed(
            Some("http://192.168.1.5:8080"),
            Some("192.168.1.5:8080")
        ));
        assert!(origin_allowed(
            Some("http://game.example.com"),
            Some("game.example.com")
        ));
        // 本机回环任意端口
        assert!(origin_allowed(
            Some("http://localhost:9999"),
            Some("127.0.0.1:8080")
        ));
        assert!(origin_allowed(
            Some("http://[::1]:8080"),
            Some("127.0.0.1:8080")
        ));
        // 公网页面 / null 源：拒绝
        assert!(!origin_allowed(
            Some("http://evil.example.com"),
            Some("127.0.0.1:8080")
        ));
        assert!(!origin_allowed(Some("null"), Some("127.0.0.1:8080")));
        // 有 Origin 但无 Host：拒绝
        assert!(!origin_allowed(Some("http://127.0.0.1:8080"), None));
    }

    #[test]
    fn http_routes() {
        let addr = spawn_server();

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let resp = recv_all(&mut s);
        assert!(
            resp.starts_with("HTTP/1.1 200 OK"),
            "{}",
            &resp[..60.min(resp.len())]
        );
        assert!(resp.contains("Content-Type: text/html; charset=utf-8"));
        assert!(resp.contains("<!DOCTYPE html>"));

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /index.html HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        assert!(recv_all(&mut s).starts_with("HTTP/1.1 200 OK"));

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /etc/passwd HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        assert!(recv_all(&mut s).starts_with("HTTP/1.1 404"));

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"POST / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let resp = recv_all(&mut s);
        assert!(
            resp.starts_with("HTTP/1.1 405"),
            "{}",
            &resp[..60.min(resp.len())]
        );
        assert!(resp.contains("Allow: GET"));
    }

    #[test]
    fn oversized_headers_rejected() {
        let addr = spawn_server();
        let mut s = TcpStream::connect(addr).unwrap();
        let big = format!("X-Pad: {}\r\n", "a".repeat(64 * 1024));
        s.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            .unwrap();
        s.write_all(big.as_bytes()).unwrap();
        s.write_all(b"\r\n").unwrap();
        let _ = s.shutdown(Shutdown::Write);
        let resp = recv_all(&mut s);
        assert!(
            resp.starts_with("HTTP/1.1 431"),
            "{}",
            &resp[..60.min(resp.len())]
        );
    }

    #[test]
    fn ws_handshake_ok_rfc6455_accept() {
        let addr = spawn_server();
        let (mut s, resp) = ws_handshake(addr, None, "127.0.0.1:8080");
        assert!(resp.starts_with("HTTP/1.1 101"), "{}", resp);
        // RFC 6455 §4.2.2 示例 accept
        assert!(resp.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
        // hello
        ws_send_text(&mut s, r#"{"type":"hello"}"#);
        let m = ws_recv(&mut s);
        assert!(m.contains(r#""type":"hello""#), "{}", m);
        assert!(m.contains(r#""engine":"rust-bitboard-expectimax""#));
        // move：走一遍完整引擎调用
        ws_send_text(
            &mut s,
            r#"{"type":"move","id":7,"board":[[2,0,0,2],[0,4,0,0],[0,0,0,0],[2,0,0,0]],"min_ms":0}"#,
        );
        let m = ws_recv(&mut s);
        assert!(m.contains(r#""type":"move""#), "{}", m);
        assert!(m.contains(r#""id":7"#));
        assert!(m.contains(r#""dir":"#));
    }

    #[test]
    fn ws_bad_input_returns_error_not_disconnect() {
        let addr = spawn_server();
        let (mut s, resp) = ws_handshake(addr, None, "127.0.0.1:8080");
        assert!(resp.starts_with("HTTP/1.1 101"));
        // 非法方块值 3：应返回 error JSON 且连接保持可用
        ws_send_text(
            &mut s,
            r#"{"type":"move","id":1,"board":[[3,0,0,0],[0,0,0,0],[0,0,0,0],[0,0,0,0]]}"#,
        );
        let m = ws_recv(&mut s);
        assert!(m.contains(r#""type":"error""#), "{}", m);
        // 非法 JSON
        ws_send_text(&mut s, "not json");
        let m = ws_recv(&mut s);
        assert!(m.contains(r#""type":"error""#), "{}", m);
        // 连接仍然可用
        ws_send_text(&mut s, r#"{"type":"hello"}"#);
        let m = ws_recv(&mut s);
        assert!(m.contains(r#""type":"hello""#), "{}", m);
    }

    #[test]
    fn ws_foreign_origin_rejected() {
        let addr = spawn_server();
        let (s, resp) = ws_handshake(addr, Some("http://evil.example.com"), "127.0.0.1:8080");
        assert!(resp.starts_with("HTTP/1.1 403"), "{}", resp);
        let _ = s.shutdown(Shutdown::Write);
    }

    #[test]
    fn ws_null_origin_rejected() {
        let addr = spawn_server();
        let (_s, resp) = ws_handshake(addr, Some("null"), "127.0.0.1:8080");
        assert!(resp.starts_with("HTTP/1.1 403"), "{}", resp);
    }

    #[test]
    fn ws_same_origin_and_localhost_allowed() {
        let addr = spawn_server();
        let (_, r1) = ws_handshake(addr, Some("http://127.0.0.1:8080"), "127.0.0.1:8080");
        assert!(r1.starts_with("HTTP/1.1 101"), "{}", r1);
        let (_, r2) = ws_handshake(addr, Some("http://localhost:9999"), "127.0.0.1:8080");
        assert!(r2.starts_with("HTTP/1.1 101"), "{}", r2);
    }

    #[test]
    fn ws_wrong_version_rejected() {
        let addr = spawn_server();
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(
            b"GET /ws HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 8\r\n\r\n",
        )
        .unwrap();
        let resp = recv_all(&mut s);
        assert!(resp.starts_with("HTTP/1.1 400"), "{}", resp);
    }

    #[test]
    fn ws_missing_connection_upgrade_rejected() {
        let addr = spawn_server();
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(
            b"GET /ws HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .unwrap();
        let resp = recv_all(&mut s);
        assert!(resp.starts_with("HTTP/1.1 400"), "{}", resp);
    }

    #[test]
    fn connection_limit_enforced() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let app = Arc::new(App {
            tables: Arc::new(MoveTables::new()),
            eval: Arc::new(Eval::build(&Weights::default())),
            pool: Arc::new(TTPool::new()),
            cores: 1,
            weights_label: "内置启发式".to_string(),
            version: "test",
            max_conns: 2,
            idle_timeout: Duration::from_secs(60),
        });
        std::thread::spawn(move || {
            let _ = serve_on(l, app);
        });
        // 占住 2 个连接（连接后不发数据，服务端线程阻塞在读首行上，占住配额）
        let mut held = Vec::new();
        for _ in 0..2 {
            let s = TcpStream::connect(addr).unwrap();
            assert!(s.peer_addr().is_ok());
            held.push(s);
        }
        std::thread::sleep(Duration::from_millis(200));
        // 第 3 个连接：服务端在 accept 时就发现超限、直接回 503 并关闭，
        // 无需等客户端发请求（此时再写反而可能吃 RST）。
        let mut s = TcpStream::connect(addr).unwrap();
        let resp = recv_all(&mut s);
        assert!(
            resp.starts_with("HTTP/1.1 503"),
            "{}",
            &resp[..60.min(resp.len())]
        );
        drop(held);
    }

    #[test]
    fn slow_drip_headers_cut_off_by_total_deadline() {
        // 慢速滴字节：每隔 60ms 发 1 字节，任何单次读都不会超时，
        // 但总时长必须被请求头总时限（这里设 300ms）截断。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let app = test_app();
        std::thread::spawn(move || {
            let _ = handle_conn_with(server, app, Duration::from_millis(300));
        });
        let start = Instant::now();
        client.write_all(b"GET / HTTP/1.1\r\nHost: x").unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(60)))
            .unwrap();
        let mut byte = [0u8; 1];
        let mut closed = false;
        // 100 轮 × 60ms = 6s，远大于 300ms 总时限
        for _ in 0..100 {
            if client.write_all(b"X").is_err() {
                closed = true; // 对端已关闭
                break;
            }
            match client.read(&mut byte) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(_) => panic!("慢速请求不应收到任何响应"),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => {
                    closed = true; // ECONNRESET 等
                    break;
                }
            }
        }
        let elapsed = start.elapsed();
        assert!(closed, "慢速滴字节的连接未被总时限切断");
        assert!(
            elapsed >= Duration::from_millis(250),
            "不应提前断开: {:?}",
            elapsed
        );
        assert!(elapsed < Duration::from_secs(5), "断开过慢: {:?}", elapsed);
    }

    #[test]
    fn ws_phase_outlives_header_deadline() {
        // 请求头总时限只约束握手阶段；升级为 WebSocket 后必须解除，
        // 否则长连接会在握手 15s 后被误杀。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let app = test_app();
        std::thread::spawn(move || {
            let _ = handle_conn_with(server, app, Duration::from_millis(200));
        });
        let resp = ws_upgrade(&mut client, None, "127.0.0.1:8080");
        assert!(resp.starts_with("HTTP/1.1 101"), "{}", resp);
        std::thread::sleep(Duration::from_millis(600));
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        ws_send_text(&mut client, r#"{"type":"hello"}"#);
        let m = ws_recv(&mut client);
        assert!(m.contains(r#""type":"hello""#), "{}", m);
    }
}
