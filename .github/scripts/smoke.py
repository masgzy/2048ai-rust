#!/usr/bin/env python3
"""ai2048 服务器端到端冒烟测试（仅标准库；Linux/macOS/Windows runner 通用）。

用法:
    smoke.py <ai2048 可执行文件> --version X.Y.Z

检查项:
  1. 命令行: --version / --help 输出；非法参数、--port 0、--config 指向不存在的文件均以退出码 2 退出
  2. 默认监听: 工作目录无 config.json 时只监听 127.0.0.1:8080，本机非回环地址不可达
  3. 配置文件: 自动加载 ./config.json；命令行 --port 覆盖配置文件
  4. HTTP 路由: / 200、/index.html 200、未知路径 404、POST 405、/ws 无升级头 400、超大请求头 431
  5. WebSocket: RFC 6455 握手（Sec-WebSocket-Accept）、hello/move 往返、非法输入返回 error 且连接
     不断、分片消息重组、ping/pong、close 回显、未掩码帧被断开；异源 Origin 与 null Origin 403、
     版本号错误 400、缺少 Connection: Upgrade 400

退出码 0 表示全部通过；任一检查失败则退出码 1。
相互独立的检查（HTTP 路由、被拒绝的握手）失败后会继续执行并一次报出全部问题；
有先后依赖的 WebSocket 流程在首个失败处停止（后续检查已无意义）。
"""

import argparse
import base64
import contextlib
import hashlib
import http.client
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
HOST = "127.0.0.1"
BOARD = [[2, 0, 0, 2], [0, 4, 0, 0], [0, 0, 0, 0], [2, 0, 0, 0]]
OP_CONT, OP_TEXT, OP_CLOSE, OP_PING, OP_PONG = 0x0, 0x1, 0x8, 0x9, 0xA


class CheckError(Exception):
    """某项检查失败。"""


class ConnectionClosed(CheckError):
    """读取时对端已关闭连接（EOF）。只在"期望被关闭"的场景视为成功。"""


# 统计：passed/failed 为单项检查数，errors 为非检查类异常（I/O 错误等）的组数，
# collect > 0 表示处于"收集模式"（检查失败不抛出，继续执行后续独立检查）。
_STATS = {"passed": 0, "failed": 0, "errors": 0, "collect": 0}


def check(cond, msg):
    _STATS["passed" if cond else "failed"] += 1
    if not cond:
        print("  FAIL " + msg, flush=True)
        if _STATS["collect"] == 0:
            raise CheckError(msg)
    else:
        print("  ok   " + msg, flush=True)


@contextlib.contextmanager
def collecting():
    """收集模式：其中的检查失败后不抛出，继续执行后面的检查。"""
    _STATS["collect"] += 1
    try:
        yield
    finally:
        _STATS["collect"] -= 1


def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind((HOST, 0))
    try:
        return s.getsockname()[1]
    finally:
        s.close()


def port_free(host, port):
    """端口当前是否可绑定。

    Unix 上带 SO_REUSEADDR 探测，与服务器自身的绑定方式一致（std 在非 Windows 平台
    总会设置该选项）：否则上一轮服务器主动关闭连接留下的 TIME_WAIT 会被误判为"被占用"。
    Windows 上服务器不设该选项，也不需要，故保持默认。
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    if sys.platform != "win32":
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        s.bind((host, port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def primary_ip():
    """本机对外主 IP（UDP connect 不发包，仅用于选路）；取不到返回 None。"""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 53))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def run_capture(cmd, cwd, timeout=30):
    p = subprocess.run(
        cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout
    )
    return p.returncode, p.stdout.decode("utf-8", "replace")


class Server:
    """服务器子进程。日志写入临时文件，检查失败时附带其内容。"""

    def __init__(self, binary, args, cwd):
        fd, self.log_path = tempfile.mkstemp(prefix="ai2048-smoke-", suffix=".log")
        self.log = os.fdopen(fd, "wb")
        self.proc = subprocess.Popen(
            [binary] + list(args), cwd=cwd, stdout=self.log, stderr=subprocess.STDOUT
        )

    def output(self):
        try:
            with open(self.log_path, "rb") as f:
                return f.read().decode("utf-8", "replace").strip()
        except OSError:
            return ""

    def wait_port(self, port, timeout=20):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise CheckError(
                    "服务器提前退出（退出码 %s）:\n%s" % (self.proc.returncode, self.output())
                )
            try:
                socket.create_connection((HOST, port), timeout=0.5).close()
                return
            except OSError:
                time.sleep(0.05)
        raise CheckError(
            "服务器 %ds 内未监听 %s:%d:\n%s" % (timeout, HOST, port, self.output())
        )

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        self.log.close()
        try:
            os.unlink(self.log_path)
        except OSError:
            pass


def http_request(port, method, path, headers=None, body=None, timeout=15):
    conn = http.client.HTTPConnection(HOST, port, timeout=timeout)
    try:
        conn.request(method, path, body=body, headers=headers or {})
        resp = conn.getresponse()
        data = resp.read()
        return resp.status, {k.lower(): v for k, v in resp.getheaders()}, data
    finally:
        conn.close()


def read_http_head(sock):
    """读到响应头结束，返回 (状态码, 小写头字典, 头之后的剩余字节)。"""
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(1024)
        if not chunk:
            break
        data += chunk
        if len(data) > 8192:
            raise CheckError("响应头超过 8KB")
    head, _, rest = data.partition(b"\r\n\r\n")
    lines = head.decode("latin-1").split("\r\n")
    parts = lines[0].split()
    status = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else None
    headers = {}
    for line in lines[1:]:
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    return status, headers, rest


class Ws:
    """最小 WebSocket 客户端：只实现冒烟测试用到的部分。"""

    def __init__(self, sock, preface=b""):
        self.s = sock
        self.buf = bytearray(preface)

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass

    def recv_exact(self, n):
        while len(self.buf) < n:
            chunk = self.s.recv(n - len(self.buf))
            if not chunk:
                raise ConnectionClosed("连接被服务端关闭（还差 %d 字节）" % (n - len(self.buf)))
            self.buf += chunk
        out = bytes(self.buf[:n])
        del self.buf[:n]
        return out

    def send_frame(self, opcode, payload, mask=True, fin=True):
        flag = 0x80 if mask else 0
        frame = bytearray([(0x80 if fin else 0) | opcode])
        n = len(payload)
        if n < 126:
            frame.append(flag | n)
        elif n < 65536:
            frame.append(flag | 126)
            frame += struct.pack(">H", n)
        else:
            frame.append(flag | 127)
            frame += struct.pack(">Q", n)
        if mask:
            key = os.urandom(4)
            frame += key
            frame += bytes(c ^ key[i % 4] for i, c in enumerate(payload))
        else:
            frame += payload
        self.s.sendall(bytes(frame))

    def recv_frame(self):
        """读一帧，返回 (fin, opcode, payload)。连接关闭抛 CheckError。"""
        b0, b1 = self.recv_exact(2)
        fin, opcode, masked = bool(b0 & 0x80), b0 & 0x0F, bool(b1 & 0x80)
        if b0 & 0x70:
            raise CheckError("服务端帧置了 RSV 位")
        if masked:
            raise CheckError("服务端帧不应掩码（RFC 6455 §5.1）")
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack(">H", self.recv_exact(2))[0]
        elif n == 127:
            n = struct.unpack(">Q", self.recv_exact(8))[0]
        return fin, opcode, self.recv_exact(n)

    def recv_message(self):
        """读一条完整消息，返回 (opcode, payload)；自动应答服务端 ping。"""
        data, opcode = b"", None
        while True:
            fin, op, payload = self.recv_frame()
            if op == OP_PING:
                self.send_frame(OP_PONG, payload)
                continue
            if op == OP_PONG:
                continue
            if op == OP_CLOSE:
                raise CheckError("服务端发送了 close 帧: %s" % payload[:2].hex())
            if op != OP_CONT:
                opcode = op
            data += payload
            if fin:
                return opcode, data

    def roundtrip(self, msg):
        """发送一条文本消息（`msg` 为 dict 或原始字符串），返回解析后的 JSON 回复。"""
        text = json.dumps(msg) if isinstance(msg, dict) else msg
        self.send_frame(OP_TEXT, text.encode("utf-8"))
        _, payload = self.recv_message()
        try:
            return json.loads(payload.decode("utf-8"))
        except ValueError:
            raise CheckError("回复不是合法 JSON: %r" % payload[:200])

    def expect_closed(self):
        """等待服务端关闭连接，返回描述；超时未关闭则抛 CheckError。"""
        try:
            while True:
                _, op, payload = self.recv_frame()
                if op == OP_CLOSE:
                    return "close 帧 code=%s" % payload[:2].hex()
        except ConnectionClosed:
            return "EOF"
        except socket.timeout:
            raise CheckError("服务端没有关闭违规连接（等待 %ss）" % self.s.gettimeout())
        except OSError:
            return "连接被重置"


def ws_handshake(port, origin=None, version="13", connection=True, timeout=15):
    """发送握手请求，返回 (Ws, 状态码, 响应头, 期望的 Sec-WebSocket-Accept)。"""
    sock = socket.create_connection((HOST, port), timeout=timeout)
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    lines = [
        "GET /ws HTTP/1.1",
        "Host: %s:%d" % (HOST, port),
        "Upgrade: websocket",
        "Sec-WebSocket-Key: " + key,
        "Sec-WebSocket-Version: " + version,
    ]
    if connection:
        lines.append("Connection: Upgrade")
    if origin is not None:
        lines.append("Origin: " + origin)
    sock.sendall(("\r\n".join(lines) + "\r\n\r\n").encode("ascii"))
    status, headers, rest = read_http_head(sock)
    digest = hashlib.sha1((key + WS_GUID).encode("ascii")).digest()
    expected = base64.b64encode(digest).decode("ascii")
    return Ws(sock, rest), status, headers, expected


def oversized_headers(port):
    """发送超过 16KB 的请求头（不带结束空行，确保服务端读完所有字节再应答），返回状态码描述。"""
    s = socket.create_connection((HOST, port), timeout=15)
    try:
        s.sendall(b"GET / HTTP/1.1\r\nHost: x\r\nX-Pad: " + b"a" * (17 * 1024) + b"\r\n")
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 4096:
            chunk = s.recv(1024)
            if not chunk:
                break
            data += chunk
        parts = data.split(b"\r\n", 1)[0].decode("latin-1").split()
        return int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else "连接关闭且无响应"
    except socket.timeout:
        return "15s 内无响应（服务器可能在无上限地等待请求头结束）"
    finally:
        s.close()


def check_cli(binary, workdir, version):
    rc, out = run_capture([binary, "--version"], workdir)
    check(rc == 0 and out.strip() == "ai2048 " + version, "--version 输出 %r" % out.strip())
    rc, out = run_capture([binary, "--help"], workdir)
    check(
        rc == 0 and all(f in out for f in ("--host", "--port", "--config")),
        "--help 列出 --host / --port / --config",
    )
    rc, _ = run_capture([binary, "--no-such-flag"], workdir)
    check(rc == 2, "未知参数退出码 2（实际 %d）" % rc)
    rc, _ = run_capture([binary, "--port", "0"], workdir)
    check(rc == 2, "--port 0 被拒绝，退出码 2（实际 %d）" % rc)
    rc, out = run_capture([binary, "--config", "missing.json"], workdir)
    check(rc == 2, "--config 指向不存在的文件时退出码 2（实际 %d）" % rc)


def check_default_bind(binary, workdir):
    if not port_free(HOST, 8080):
        print("  SKIP 8080 端口已被占用，跳过默认监听地址检查")
        return
    srv = Server(binary, [], workdir)
    try:
        srv.wait_port(8080)
        status, _, _ = http_request(8080, "GET", "/")
        check(status == 200, "无 config.json 时默认监听 127.0.0.1:8080（HTTP %s）" % status)
        lan = primary_ip()
        if lan and not lan.startswith("127."):
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(10)
            try:
                rc = s.connect_ex((lan, 8080))
            finally:
                s.close()
            check(rc != 0, "默认配置下非回环地址 %s:8080 不可达（connect 返回 %d）" % (lan, rc))
        else:
            print("  SKIP 取不到非回环地址，跳过外地址不可达检查")
    finally:
        srv.stop()


def check_config(binary, workdir):
    cfg_port, cli_port = free_port(), free_port()
    while cli_port == cfg_port:
        cli_port = free_port()
    cfg = {"host": HOST, "port": cfg_port, "max_connections": 16, "idle_timeout_secs": 30}
    with open(os.path.join(workdir, "config.json"), "w", encoding="utf-8") as f:
        json.dump(cfg, f)

    srv = Server(binary, [], workdir)
    try:
        srv.wait_port(cfg_port)
        check(True, "自动加载 ./config.json，监听端口 %d" % cfg_port)
    finally:
        srv.stop()

    srv = Server(binary, ["--port", str(cli_port)], workdir)
    try:
        srv.wait_port(cli_port)
        free = port_free(HOST, cfg_port)
        check(
            free,
            "命令行 --port 覆盖配置文件（监听 %d，配置文件中的 %d 未被监听）"
            % (cli_port, cfg_port),
        )
    finally:
        srv.stop()


def check_http(port):
    # 各请求相互独立：全部执行，一次报出所有问题
    with collecting():
        status, headers, body = http_request(port, "GET", "/")
        check(
            status == 200 and headers.get("content-type", "").startswith("text/html"),
            "GET / -> 200 text/html（实际 %s）" % status,
        )
        check(
            b"2048" in body and b"<html" in body.lower(),
            "首页为内嵌的 2048 页面（%d 字节）" % len(body),
        )
        check(headers.get("cache-control") == "no-store", "首页响应带 Cache-Control: no-store")
        status, _, _ = http_request(port, "GET", "/index.html")
        check(status == 200, "GET /index.html -> 200（实际 %s）" % status)
        status, _, _ = http_request(port, "GET", "/etc/passwd")
        check(status == 404, "GET /etc/passwd -> 404（实际 %s）" % status)
        status, headers, _ = http_request(port, "POST", "/")
        check(
            status == 405 and "GET" in headers.get("allow", ""),
            "POST / -> 405 且 Allow: GET（实际 %s，Allow: %s）"
            % (status, headers.get("allow")),
        )
        status, _, _ = http_request(port, "GET", "/ws")
        check(status == 400, "GET /ws（无升级头）-> 400（实际 %s）" % status)
        status = oversized_headers(port)
        check(status == 431, "超过 16KB 的请求头 -> 431（实际 %s）" % status)


def check_ws_session(port, version):
    """主流程：有先后依赖，首个失败处停止。"""
    ws, status, headers, expected = ws_handshake(port, origin="http://%s:%d" % (HOST, port))
    try:
        check(status == 101, "同源握手 -> 101（实际 %s）" % status)
        check(
            headers.get("sec-websocket-accept") == expected,
            "Sec-WebSocket-Accept 与 RFC 6455 计算值一致",
        )
        check(headers.get("upgrade", "").lower() == "websocket", "响应带 Upgrade: websocket")

        hello = ws.roundtrip({"type": "hello"})
        check(
            hello.get("type") == "hello" and hello.get("version") == version,
            "hello -> 版本 %s" % hello.get("version"),
        )
        check(
            isinstance(hello.get("cores"), int) and hello["cores"] >= 1,
            "hello 报告可用核数 %s" % hello.get("cores"),
        )

        move = ws.roundtrip({"type": "move", "id": 7, "board": BOARD, "min_ms": 0})
        check(
            move.get("type") == "move" and move.get("id") == 7,
            "move -> type=move、id=7 的回复",
        )
        check(move.get("dir") in (0, 1, 2, 3), "move 方向合法: %s" % move.get("dir"))
        check(
            isinstance(move.get("nodes"), int) and move["nodes"] > 0,
            "move 实际执行了搜索（深度 %s，%s 节点，%s ms）"
            % (move.get("depth"), move.get("nodes"), move.get("ms")),
        )

        bad = ws.roundtrip({"type": "move", "id": 8, "board": [[3, 0, 0, 0], [0] * 4, [0] * 4, [0] * 4]})
        check(
            bad.get("type") == "error" and bad.get("id") == 8,
            "非法方块值 -> error（id=8）: %s" % bad.get("error"),
        )
        check(ws.roundtrip("{not json").get("type") == "error", "非法 JSON -> error")
        check(ws.roundtrip({"type": "nope"}).get("type") == "error", "未知消息类型 -> error")
        check(ws.roundtrip({"type": "hello"}).get("type") == "hello", "多次出错后连接仍然可用")

        ws.send_frame(OP_PING, b"ping!")
        _, op, payload = ws.recv_frame()
        check(op == OP_PONG and payload == b"ping!", "ping -> pong 回显载荷")

        text = json.dumps({"type": "hello"}).encode("utf-8")
        ws.send_frame(OP_TEXT, text[:5], fin=False)
        ws.send_frame(OP_CONT, text[5:], fin=True)
        _, payload = ws.recv_message()
        check(json.loads(payload.decode("utf-8")).get("type") == "hello", "分片消息被正确重组")

        ws.send_frame(OP_CLOSE, struct.pack(">H", 1000))
        _, op, _ = ws.recv_frame()
        check(op == OP_CLOSE, "close 帧被回显")
        # expect_closed 只有在真正等到对端关闭时才返回，否则抛 CheckError
        closed = ws.expect_closed()
        check(True, "close 之后服务端结束连接（%s）" % closed)
    finally:
        ws.close()


def check_ws_unmasked_frame(port):
    """无 Origin 的非浏览器客户端应被允许；未掩码的客户端帧违反 RFC 6455 §5.1，必须断开。"""
    ws, status, _, _ = ws_handshake(port)
    try:
        check(status == 101, "无 Origin 的非浏览器客户端 -> 101（实际 %s）" % status)
        ws.send_frame(OP_TEXT, b'{"type":"hello"}', mask=False)
        # expect_closed 只有在真正等到对端关闭时才返回，否则抛 CheckError
        check(True, "未掩码的客户端帧 -> 服务端断开连接（%s）" % ws.expect_closed())
    finally:
        ws.close()


def check_ws_rejected_handshakes(port):
    # 各握手相互独立：全部执行，一次报出所有问题
    with collecting():
        for name, kwargs, want in [
            ("异源 Origin", dict(origin="http://evil.example.com"), 403),
            ("null Origin", dict(origin="null"), 403),
            ("Sec-WebSocket-Version: 8", dict(version="8"), 400),
            ("缺少 Connection: Upgrade", dict(connection=False), 400),
        ]:
            ws, status, _, _ = ws_handshake(port, **kwargs)
            ws.close()
            check(status == want, "%s -> %d（实际 %s）" % (name, want, status))


def run_section(title, fn, *args):
    print("[%s]" % title, flush=True)
    try:
        fn(*args)
        return True
    except CheckError as exc:
        # 单项检查失败已由 check() 计数并打印
        print("  （本组在此中断: %s）" % exc, flush=True)
        return False
    except OSError as exc:  # 含 socket.timeout
        _STATS["errors"] += 1
        print("  FAIL I/O 错误: %r" % (exc,), flush=True)
        return False


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(
        description="ai2048 服务器端到端冒烟测试",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("binary", help="ai2048 可执行文件路径")
    ap.add_argument("--version", required=True, help="期望的版本号（须与 Cargo.toml 一致）")
    args = ap.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        sys.exit("smoke: 找不到可执行文件 %s" % binary)

    root = tempfile.mkdtemp(prefix="ai2048-smoke-")
    plain = os.path.join(root, "plain")
    configured = os.path.join(root, "configured")
    os.makedirs(plain)
    os.makedirs(configured)
    # HTTP / WebSocket 检查用的服务器：显式 --config + 空闲端口
    port = free_port()
    cfg_path = os.path.join(configured, "smoke.json")
    with open(cfg_path, "w", encoding="utf-8") as f:
        json.dump({"host": HOST, "port": port, "max_connections": 16, "idle_timeout_secs": 30}, f)

    try:
        run_section("命令行", check_cli, binary, plain, args.version)
        run_section("默认监听地址", check_default_bind, binary, plain)
        run_section("配置文件", check_config, binary, configured)
        srv = Server(binary, ["--config", cfg_path], configured)
        try:
            if run_section("启动服务器（--config）", srv.wait_port, port):
                run_section("HTTP 路由", check_http, port)
                run_section("WebSocket 会话", check_ws_session, port, args.version)
                run_section("WebSocket 违规帧", check_ws_unmasked_frame, port)
                run_section("WebSocket 握手拒绝", check_ws_rejected_handshakes, port)
            else:
                print("  （服务器未能启动，跳过 HTTP / WebSocket 检查）", flush=True)
        finally:
            srv.stop()
    finally:
        shutil.rmtree(root, ignore_errors=True)

    failed = _STATS["failed"] + _STATS["errors"]
    print("-" * 60, flush=True)
    print(
        "冒烟测试: %d 项通过，%d 项失败（另有 %d 组因 I/O 错误中断）"
        % (_STATS["passed"], _STATS["failed"], _STATS["errors"]),
        flush=True,
    )
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
