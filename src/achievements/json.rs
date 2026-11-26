//! Minimal JSON reader/writer (no external crates). Objects are `BTreeMap`s, so
//! output is always key-sorted: that is the "canonical JSON" used for the
//! same-content check. Numbers are `f64` (exact for integers below 2^53).

use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if n.is_finite() && *n >= 0.0 => Some(*n as u64),
            Json::Bool(b) => Some(*b as u64),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Obj(m) => Some(m),
            _ => None,
        }
    }

    /// Compact serialisation, keys sorted.
    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => write_num(*n, out),
            Json::Str(s) => write_str(s, out),
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_string(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }
}

pub fn write_num(n: f64, out: &mut String) {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        let _ = write!(out, "{}", n as i64);
    } else if n.is_finite() {
        let _ = write!(out, "{}", n);
    } else {
        out.push('0');
    }
}

pub fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse one JSON document. `None` on any syntax error or trailing garbage.
pub fn parse(text: &str) -> Option<Json> {
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i == p.b.len() {
        Some(v)
    } else {
        None
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn lit(&mut self, word: &str, v: Json) -> Option<Json> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Some(v)
        } else {
            None
        }
    }

    fn value(&mut self, depth: u32) -> Option<Json> {
        if depth > 32 {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut m = BTreeMap::new();
                if self.eat(b'}') {
                    return Some(Json::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    if !self.eat(b':') {
                        return None;
                    }
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Some(Json::Obj(m));
                    }
                    return None;
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                if self.eat(b']') {
                    return Some(Json::Arr(a));
                }
                loop {
                    a.push(self.value(depth + 1)?);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Some(Json::Arr(a));
                    }
                    return None;
                }
            }
            b'"' => Some(Json::Str(self.string()?)),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).ok()?;
        s.parse::<f64>().ok().map(Json::Num)
    }

    fn hex4(&mut self) -> Option<u32> {
        let s = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        let v = u32::from_str_radix(s, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // surrogate pair
                                if self.b.get(self.i) == Some(&b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    cp = 0x10000
                                        + ((cp - 0xD800) << 10)
                                        + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                } else {
                                    return None;
                                }
                            }
                            char::from_u32(cp)?
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_sorted() {
        let j = parse(r#"{"b":[1,2.5,true,null],"a":{"x":"h\"i\né"}}"#).unwrap();
        let s = j.to_string();
        assert_eq!(
            s,
            "{\"a\":{\"x\":\"h\\\"i\\n\u{e9}\"},\"b\":[1,2.5,true,null]}"
        );
        assert_eq!(parse(&s).unwrap(), j);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("{").is_none());
        assert!(parse("{} x").is_none());
        assert!(parse("").is_none());
        assert!(parse("[1,]").is_none());
    }

    #[test]
    fn big_integers_stay_exact() {
        let j = parse("{\"n\":5000000000000}").unwrap();
        assert_eq!(j.get("n").unwrap().as_u64(), Some(5_000_000_000_000));
        assert_eq!(j.to_string(), "{\"n\":5000000000000}");
    }
}
