//! A small JSON reader for `jfr print --json`: parses one value at a time from a byte slice,
//! so a recording's events array is read event by event without building the whole document.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn arr(&self) -> &[Value] {
        match self {
            Value::Arr(a) => a,
            _ => &[],
        }
    }
}

pub struct Parser<'a> {
    s: &'a [u8],
    pub at: usize,
}

impl<'a> Parser<'a> {
    pub fn new(s: &'a [u8]) -> Parser<'a> {
        Parser { s, at: 0 }
    }

    fn ws(&mut self) {
        while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("JSON: {what} at byte {}", self.at))
    }

    /// Moves past the next `"key":` at any depth, before an array or object it names.
    pub fn seek_key(&mut self, key: &str) -> bool {
        let pat = format!("\"{key}\"");
        match self.s[self.at..].windows(pat.len()).position(|w| w == pat.as_bytes()) {
            Some(i) => {
                self.at += i + pat.len();
                self.ws();
                if self.s.get(self.at) == Some(&b':') {
                    self.at += 1;
                }
                true
            }
            None => false,
        }
    }

    /// Consumes `c` after whitespace.
    pub fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.at) == Some(&c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    pub fn value(&mut self) -> Result<Value, String> {
        self.ws();
        match self.s.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut fields = Vec::new();
                if self.eat(b'}') {
                    return Ok(Value::Obj(fields));
                }
                loop {
                    self.ws();
                    let Value::Str(k) = self.string()? else { unreachable!() };
                    if !self.eat(b':') {
                        return self.err("':' expected");
                    }
                    fields.push((k, self.value()?));
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(Value::Obj(fields));
                    }
                    return self.err("',' or '}' expected");
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Ok(Value::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(Value::Arr(items));
                    }
                    return self.err("',' or ']' expected");
                }
            }
            Some(b'"') => self.string(),
            Some(b't') if self.s[self.at..].starts_with(b"true") => {
                self.at += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.s[self.at..].starts_with(b"false") => {
                self.at += 5;
                Ok(Value::Bool(false))
            }
            Some(b'n') if self.s[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Value::Null)
            }
            Some(c) if c.is_ascii_digit() || *c == b'-' => {
                let start = self.at;
                while self.at < self.s.len() && matches!(self.s[self.at], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.at]).unwrap_or("");
                text.parse().map(Value::Num).or_else(|_| self.err("a bad number"))
            }
            _ => self.err("a value expected"),
        }
    }

    fn string(&mut self) -> Result<Value, String> {
        if self.s.get(self.at) != Some(&b'"') {
            return self.err("a string expected");
        }
        self.at += 1;
        let mut out = Vec::new();
        while let Some(&c) = self.s.get(self.at) {
            self.at += 1;
            match c {
                b'"' => return Ok(Value::Str(String::from_utf8_lossy(&out).into_owned())),
                b'\\' => {
                    let e = *self.s.get(self.at).ok_or("JSON: a truncated escape")?;
                    self.at += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.at..self.at + 4).ok_or("JSON: a truncated escape")?).unwrap_or("");
                            self.at += 4;
                            let ch = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
        self.err("an unterminated string")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values() {
        let mut p = Parser::new(br#" {"a": [1, -2.5e1, "x\/yA"], "b": {"c": true, "d": null}, "e": false} "#);
        let v = p.value().unwrap();
        assert_eq!(v.get("a").unwrap().arr()[1].num(), Some(-25.0));
        assert_eq!(v.get("a").unwrap().arr()[2].str(), Some("x/yA"));
        assert_eq!(v.get("b").unwrap().get("c"), Some(&Value::Bool(true)));
        assert_eq!(v.get("e"), Some(&Value::Bool(false)));
    }

    #[test]
    fn reads_an_array_item_by_item() {
        let text = br#"{"recording": {"events": [{"n": 1}, {"n": 2}]}}"#;
        let mut p = Parser::new(text);
        assert!(p.seek_key("events"));
        assert!(p.eat(b'['));
        let mut n = Vec::new();
        loop {
            n.push(p.value().unwrap().get("n").unwrap().num().unwrap());
            if !p.eat(b',') {
                break;
            }
        }
        assert!(p.eat(b']'));
        assert_eq!(n, vec![1.0, 2.0]);
    }
}
