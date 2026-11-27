//! RFC6455 WebSocket 服务端实现（零依赖：SHA1、base64、帧编解码）。
//!
//! 协议严格性（安全审查后加严）：
//! - 客户端到服务端的帧**必须**带掩码（RFC 6455 §5.1），未掩码直接断开；
//! - RSV 位必须为 0（未协商任何扩展）；
//! - 控制帧必须 FIN=1 且载荷 <=125 字节，且不可分片（§5.5）；
//! - 分片消息按累计长度设上限，防止用大量小分片绕过单帧上限做内存放大。

use std::io::{Read, Write};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// 单帧载荷上限（1MB）。
const MAX_FRAME: u64 = 1 << 20;
/// 控制帧载荷上限（RFC 6455 §5.5）。
const MAX_CONTROL: u64 = 125;

/// 计算握手响应的 Sec-WebSocket-Accept。
pub fn accept_key(key: &str) -> String {
    let data = format!("{}{}", key.trim(), WS_GUID);
    b64(&sha1(data.as_bytes()))
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 读取一帧。协议错误/连接结束返回 None（调用方直接关闭即可）。
fn read_frame(r: &mut impl Read) -> Option<(bool, u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h).ok()?;
    let fin = h[0] & 0x80 != 0;
    if h[0] & 0x70 != 0 {
        return None; // RSV 位被置位：未协商任何扩展
    }
    let opcode = h[0] & 0x0F;
    let masked = h[1] & 0x80 != 0;
    let mut len = (h[1] & 0x7F) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        r.read_exact(&mut b).ok()?;
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        r.read_exact(&mut b).ok()?;
        len = u64::from_be_bytes(b);
    }
    if !masked {
        return None; // §5.1：客户端帧必须掩码
    }
    if len > MAX_FRAME {
        return None; // 单帧上限 1MB
    }
    if matches!(opcode, 0x8..=0xA) && (!fin || len > MAX_CONTROL) {
        return None; // 控制帧：不可分片且 <=125 字节
    }
    let mut m = [0u8; 4];
    r.read_exact(&mut m).ok()?;
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).ok()?;
    for (i, p) in payload.iter_mut().enumerate() {
        *p ^= m[i % 4];
    }
    Some((fin, opcode, payload))
}

/// 读取一条完整消息（处理分片/ping/close）。
/// `max_len` 为分片累计上限：超出视为协议错误返回 None，
/// 防止"每帧 1MB 以内、累计无上限"的内存放大。
/// 返回 None 表示连接结束。
pub fn read_message(r: &mut impl Read, w: &mut impl Write, max_len: usize) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut started = false;
    loop {
        let (fin, op, payload) = read_frame(r)?;
        match op {
            0x8 => {
                // close：回应 close 帧后结束
                let _ = write_frame(w, 0x8, &payload);
                return None;
            }
            0x9 => {
                let _ = write_frame(w, 0xA, &payload); // ping → pong
                continue;
            }
            0xA => continue, // pong
            0x1 | 0x2 => {
                if started {
                    return None; // 上一条分片消息未结束就开新消息
                }
                if payload.len() > max_len {
                    return None;
                }
                buf = payload;
                started = true;
                if fin {
                    return Some(buf);
                }
            }
            0x0 => {
                if !started {
                    return None; // 没有起始帧的续帧
                }
                if buf.len() + payload.len() > max_len {
                    return None;
                }
                buf.extend_from_slice(&payload);
                if fin {
                    return Some(buf);
                }
            }
            _ => return None, // 未知 opcode：按协议失败处理
        }
    }
}

pub fn write_frame(w: &mut impl Write, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut hdr = Vec::with_capacity(10);
    hdr.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
        hdr.push(len as u8);
    } else if len < 65536 {
        hdr.push(126);
        hdr.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        hdr.push(127);
        hdr.extend_from_slice(&(len as u64).to_be_bytes());
    }
    w.write_all(&hdr)?;
    w.write_all(payload)?;
    w.flush()
}

pub fn write_text(w: &mut impl Write, s: &str) -> std::io::Result<()> {
    write_frame(w, 0x1, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写一帧客户端样式的掩码帧（测试用）。`fin` 控制 FIN 位（分片场景用 false）。
    fn write_masked_frame(
        w: &mut impl Write,
        fin: bool,
        opcode: u8,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let mask = [0x0A, 0x0B, 0x0C, 0x0D];
        let mut frame = vec![if fin { 0x80 | opcode } else { opcode }];
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
        w.write_all(&frame)
    }

    #[test]
    fn sha1_rfc3174_abc() {
        // SHA1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        let d = sha1(b"abc");
        assert_eq!(
            d.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn sha1_long() {
        // SHA1("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")
        let d = sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
        assert_eq!(
            d.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn accept_key_rfc6455() {
        // RFC 6455 §4.2.2 示例
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn b64_rfc4648() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn frame_roundtrip_masked() {
        // 读取路径只接受掩码帧（read_frame 强制 §5.1），这里走客户端方向。
        let mut buf = Vec::new();
        write_masked_frame(&mut buf, true, 0x1, "你好 ws".as_bytes()).unwrap();
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        let msg = read_message(&mut rd, &mut wr, 65536).unwrap_or_default();
        assert_eq!(String::from_utf8(msg).unwrap(), "你好 ws");
    }

    #[test]
    fn server_write_frame_unmasked() {
        // write_frame 产出的服务端帧：未掩码、FIN=1
        let mut buf = Vec::new();
        write_text(&mut buf, "hello").unwrap();
        assert_eq!(buf[0], 0x81);
        assert_eq!(buf[1], 0x05);
        assert_eq!(&buf[2..], b"hello");
    }

    #[test]
    fn unmasked_client_frame_rejected() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 0x1, b"hi").unwrap(); // 未掩码
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 65536).is_none());
    }

    #[test]
    fn rsv_bits_rejected() {
        let mut frame = vec![0x81 | 0x40, 0x80 | 2, 1, 2, 3, 4]; // RSV1 置位 + 2 字节载荷
        frame.extend_from_slice(&[b'h' ^ 1, b'i' ^ 2]);
        let mut rd: &[u8] = &frame;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 65536).is_none());
    }

    #[test]
    fn fragmented_message_reassembled() {
        let mut buf = Vec::new();
        let full = "0123456789abcdef".as_bytes();
        write_masked_frame(&mut buf, false, 0x1, &full[..8]).unwrap(); // 起始帧，FIN=0
        write_masked_frame(&mut buf, true, 0x0, &full[8..]).unwrap(); // 续帧，FIN=1
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        let msg = read_message(&mut rd, &mut wr, 65536).unwrap();
        assert_eq!(msg, full);
    }

    #[test]
    fn fragmented_total_length_capped() {
        // 每帧都很小，但累计超过 max_len：必须当场断开，
        // 而不是等整条消息组装完再由调用方检查（内存放大防护的核心）。
        let mut buf = Vec::new();
        write_masked_frame(&mut buf, false, 0x1, &[b'a'; 600]).unwrap(); // FIN=0 起始
        write_masked_frame(&mut buf, true, 0x0, &[b'b'; 600]).unwrap(); // 续帧，累计 1200 > 1024
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 1024).is_none());
    }

    #[test]
    fn oversized_single_frame_rejected() {
        // 单帧载荷超出 1MB 上限
        let mut buf = Vec::new();
        write_masked_frame(&mut buf, true, 0x1, &[0u8; (1 << 20) + 1]).unwrap();
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 1 << 21).is_none());
    }

    #[test]
    fn oversized_control_frame_rejected() {
        // ping 载荷 >125 字节：协议错误
        let mut buf = Vec::new();
        write_masked_frame(&mut buf, true, 0x9, &[0u8; 126]).unwrap();
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 65536).is_none());
    }

    #[test]
    fn continuation_without_start_rejected() {
        let mut buf = Vec::new();
        write_masked_frame(&mut buf, true, 0x0, b"oops").unwrap();
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 65536).is_none());
    }

    #[test]
    fn close_frame_echoed() {
        let mut buf = Vec::new();
        write_masked_frame(&mut buf, true, 0x8, &[0x03, 0xE8]).unwrap(); // 1000
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        assert!(read_message(&mut rd, &mut wr, 65536).is_none());
        // 响应里应有 close 回显
        assert_eq!(wr[0], 0x88);
        assert_eq!(&wr[2..], &[0x03, 0xE8]);
    }
}
