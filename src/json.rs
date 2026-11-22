//! 极简 JSON：解析 + 转义。只覆盖本服务器协议需要的子集，零依赖。

#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(s: &str) -> Result<Json, String> {
        let mut p = Parser { b: s.as_bytes(), i: 0 };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("第 {} 字节处有多余字符", p.i));
        }
        Ok(v)
    }

    pub fn get(&self, k: &str) -> Option<&Json> {
        if let Json::Obj(m) = self {
            m.iter().find(|(kk, _)| kk == k).map(|(_, v)| v)
        } else {
            None
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Json::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        if let Json::Num(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let Json::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        if let Json::Arr(a) = self {
            Some(a)
        } else {
            None
        }
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len()
            && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 64 {
            return Err("JSON 嵌套过深".to_string());
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("第 {} 字节出现意外字符 '{}'", self.i, c as char)),
            None => Err("输入提前结束".to_string()),
        }
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(format!("第 {} 字节处字面量错误", self.i))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E') {
                self.i += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        s.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("非法数字 '{}'", s))
    }

    fn string(&mut self) -> Result<String, String> {
        // 假定 peek == '"'
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("字符串未闭合")?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self.peek().ok_or("转义序列未闭合")?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if self.i + 4 > self.b.len() {
                                return Err("\\u 转义不完整".to_string());
                            }
                            let hex = std::str::from_utf8(&self.b[self.i..self.i + 4])
                                .map_err(|_| "\\u 编码错误".to_string())?;
                            let cp = u32::from_str_radix(hex, 16)
                                .map_err(|_| "\\u 编码错误".to_string())?;
                            self.i += 4;
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(format!("未知转义 \\{}", e as char)),
                    }
                }
                _ => {
                    // 收集完整 UTF-8 字符
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    if start + len > self.b.len() {
                        return Err("UTF-8 序列不完整".to_string());
                    }
                    let s = std::str::from_utf8(&self.b[start..start + len])
                        .map_err(|_| "UTF-8 编码错误".to_string())?;
                    out.push_str(s);
                    self.i = start + len;
                }
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.i += 1; // '['
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth + 1)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                _ => return Err(format!("第 {} 字节处数组缺少 ]", self.i)),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.i += 1; // '{'
        let mut out: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(format!("第 {} 字节处对象键应为字符串", self.i));
            }
            let k = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(format!("第 {} 字节处缺少 :", self.i));
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            out.push((k, v));
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(out));
                }
                _ => return Err(format!("第 {} 字节处对象缺少 }}", self.i)),
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_board_and_fields() {
        let j = Json::parse(
            r#"{"type":"move","id":3,"min_ms":0,"board":[[2,0,0,0],[0,4,0,0],[2,0,0,0],[0,0,0,2]]}"#,
        )
        .unwrap();
        assert_eq!(j.get("type").unwrap().as_str().unwrap(), "move");
        assert_eq!(j.get("id").unwrap().as_u64().unwrap(), 3);
        let b = j.get("board").unwrap().as_arr().unwrap();
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].as_arr().unwrap()[0].as_u64().unwrap(), 2);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("[1,2,").is_err());
        assert!(Json::parse("hello").is_err());
        assert!(Json::parse("{\"a\":1} junk").is_err());
    }

    #[test]
    fn unicode_and_escapes() {
        let j = Json::parse(r#"{"s":"a\"b\n中文"}"#).unwrap();
        assert_eq!(j.get("s").unwrap().as_str().unwrap(), "a\"b\n中文");
    }
}
