//! RFC6455 WebSocket 服务端实现（零依赖：SHA1、base64、帧编解码）。

use std::io::{Read, Write};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

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
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// 读取一帧。连接结束/出错返回 None（调用方直接关闭即可）。
fn read_frame(r: &mut impl Read) -> Option<(bool, u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h).ok()?;
    let fin = h[0] & 0x80 != 0;
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
    if len > (1 << 20) {
        return None; // 单帧上限 1MB
    }
    let mask = if masked {
        let mut m = [0u8; 4];
        r.read_exact(&mut m).ok()?;
        Some(m)
    } else {
        None
    };
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).ok()?;
    if let Some(m) = mask {
        for (i, p) in payload.iter_mut().enumerate() {
            *p ^= m[i % 4];
        }
    }
    Some((fin, opcode, payload))
}

/// 读取一条完整消息（处理分片/ping/close）。返回 None 表示连接结束。
pub fn read_message(r: &mut impl Read, w: &mut impl Write) -> Option<Vec<u8>> {
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
                    return None; // 协议错误
                }
                buf = payload;
                started = true;
                if fin {
                    return Some(buf);
                }
            }
            0x0 => {
                if !started {
                    return None;
                }
                buf.extend_from_slice(&payload);
                if fin {
                    return Some(buf);
                }
            }
            _ => {}
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
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        write_text(&mut buf, "你好 ws").unwrap();
        let mut rd: &[u8] = &buf;
        let mut wr = Vec::new();
        let msg = read_message(&mut rd, &mut wr).unwrap_or_default();
        assert_eq!(String::from_utf8(msg).unwrap(), "你好 ws");
    }
}
