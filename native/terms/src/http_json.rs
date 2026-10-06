//! HTTP-only insertion-ordered JSON. Never use this representation for stored JSON/fingerprints.
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug)]
pub enum Json {
    Scalar(Value),
    Float(f64),
    Integer(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}
impl Json {
    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Json)>) -> Self {
        let mut value = Self::Object(Vec::new());
        for (key, v) in fields {
            value.set(key, v);
        }
        value
    }
    // Python dict.update keeps the original position of overwritten fields.
    pub fn set(&mut self, key: impl Into<String>, value: Json) {
        let key = key.into();
        if let Self::Object(fields) = self {
            if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == &key) {
                *v = value;
            } else {
                fields.push((key, value));
            }
        }
    }
    pub fn extend(&mut self, other: Json) {
        if let Self::Object(fields) = other {
            for (k, v) in fields {
                self.set(k, v);
            }
        }
    }
    pub fn get(&self, key: &str) -> Option<&Json> {
        if let Self::Object(fields) = self {
            fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    }
    pub fn text(&self) -> &str {
        if let Self::Scalar(Value::String(v)) = self {
            v
        } else {
            ""
        }
    }
    pub fn integer(&self) -> i64 {
        match self {
            Self::Scalar(v) => v.as_i64().unwrap_or(0),
            Self::Integer(v) => v.parse().unwrap_or(0),
            _ => 0,
        }
    }
    pub fn dump(&self) -> String {
        let mut out = Vec::new();
        self.write(&mut out);
        String::from_utf8(out).expect("JSON is UTF-8")
    }
    // One output buffer; large pages must not allocate/join a string for every
    // nested field. Retain the existing Python float formatter and separators.
    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Scalar(Value::String(v)) => serde_json::to_writer(out, v).expect("JSON string"),
            Self::Scalar(v) => out.extend_from_slice(crate::character_json::dump(v, false).as_bytes()),
            Self::Integer(v) => out.extend_from_slice(v.as_bytes()),
            Self::Float(v) if v.is_nan() => out.extend_from_slice(b"NaN"),
            Self::Float(v) if *v == f64::INFINITY => out.extend_from_slice(b"Infinity"),
            Self::Float(v) if *v == f64::NEG_INFINITY => out.extend_from_slice(b"-Infinity"),
            Self::Float(v) => out.extend_from_slice(crate::character_json::dump(&Value::from(*v), false).as_bytes()),
            Self::Array(v) => {
                out.push(b'[');
                for (i, v) in v.iter().enumerate() {
                    if i != 0 {
                        out.extend_from_slice(b", ");
                    }
                    v.write(out);
                }
                out.push(b']');
            }
            Self::Object(v) => {
                out.push(b'{');
                for (i, (k, v)) in v.iter().enumerate() {
                    if i != 0 {
                        out.extend_from_slice(b", ");
                    }
                    serde_json::to_writer(&mut *out, k).expect("JSON key");
                    out.extend_from_slice(b": ");
                    v.write(out);
                }
                out.push(b'}');
            }
        }
    }
}
impl From<Value> for Json {
    fn from(v: Value) -> Self {
        match v {
            Value::Array(v) => Self::Array(v.into_iter().map(Self::from).collect()),
            Value::Object(v) => Self::Object(v.into_iter().map(|(k, v)| (k, Self::from(v))).collect()),
            v => Self::Scalar(v),
        }
    }
}
impl From<&str> for Json {
    fn from(v: &str) -> Self {
        Self::Scalar(Value::String(v.into()))
    }
}
impl From<String> for Json {
    fn from(v: String) -> Self {
        Self::from(v.as_str())
    }
}
impl From<i64> for Json {
    fn from(v: i64) -> Self {
        Self::Scalar(v.into())
    }
}
impl From<bool> for Json {
    fn from(v: bool) -> Self {
        Self::Scalar(v.into())
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Self::Scalar(Value::Null))
    }
}

// Deserialize maps without serde_json::Map's sorted order (including nested evidence objects).
impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = Json;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "JSON")
            }
            fn visit_unit<E>(self) -> Result<Json, E> {
                Ok(None::<String>.into())
            }
            fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
                Ok(v.into())
            }
            fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
                Ok(v.into())
            }
            fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
                Ok(Json::Scalar(v.into()))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Json, E> {
                Ok(Json::Float(v))
            }
            fn visit_str<E>(self, v: &str) -> Result<Json, E> {
                Ok(v.into())
            }
            fn visit_string<E>(self, v: String) -> Result<Json, E> {
                Ok(v.into())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element()? {
                    v.push(x)
                }
                Ok(Json::Array(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut v = Json::Object(Vec::new());
                while let Some((k, x)) = a.next_entry::<String, Json>()? {
                    v.set(k, x)
                }
                Ok(v)
            }
        }
        d.deserialize_any(Ordered)
    }
}
/// Evidence uses Python json.loads: non-finite constants and arbitrary-size integers are valid.
/// Decode only HTTP values; serde's global map/number features (and persistence) remain unchanged.
pub fn parse_python(input: &str) -> crate::Result<Json> {
    parse(input, false)
}
/// Request errors follow CPython JSONDecodeError; evidence callers retain the original refusal behavior.
pub(crate) fn parse_request(input: &str) -> crate::Result<Json> {
    parse(input, true)
}
fn parse(input: &str, request_errors: bool) -> crate::Result<Json> {
    struct Parser<'a> {
        s: &'a str,
        at: usize,
        request_errors: bool,
    }
    impl Parser<'_> {
        fn ws(&mut self) {
            while self.s.as_bytes().get(self.at).is_some_and(|b| b" \t\r\n".contains(b)) {
                self.at += 1;
            }
        }
        fn take(&mut self, b: u8) -> bool {
            self.ws();
            if self.s.as_bytes().get(self.at) == Some(&b) {
                self.at += 1;
                true
            } else {
                false
            }
        }
        fn bad<T>(&self) -> crate::Result<T> {
            self.fail("Expecting value", self.at)
        }
        fn fail<T>(&self, message: &str, at: usize) -> crate::Result<T> {
            if !self.request_errors {
                return Err(crate::Error::Invalid("无效 JSON 证据".into()));
            }
            let before = &self.s[..at];
            let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            Err(crate::Error::Invalid(format!(
                "{message}: line {line} column {column} (char {})",
                before.chars().count()
            )))
        }
        // CPython's scanner consumes a valid numeric prefix, leaving punctuation errors to the container.
        fn request_number_end(&self) -> Option<usize> {
            let bytes = self.s.as_bytes();
            let mut at = self.at + usize::from(bytes.get(self.at) == Some(&b'-'));
            match bytes.get(at) {
                Some(b'0') => at += 1,
                Some(b'1'..=b'9') => {
                    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                        at += 1;
                    }
                }
                _ => return None,
            }
            if bytes.get(at) == Some(&b'.') && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
                at += 2;
                while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
            }
            if bytes.get(at).is_some_and(|b| b"eE".contains(b)) {
                let mut end = at + 1;
                if bytes.get(end).is_some_and(|b| b"+-".contains(b)) {
                    end += 1;
                }
                if bytes.get(end).is_some_and(u8::is_ascii_digit) {
                    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                        end += 1;
                    }
                    at = end;
                }
            }
            Some(at)
        }
        fn string(&mut self) -> crate::Result<String> {
            self.ws();
            let start = self.at;
            if !self.take(b'"') {
                return self.fail("Expecting property name enclosed in double quotes", self.at);
            }
            let mut escaped = false;
            while let Some(b) = self.s.as_bytes().get(self.at) {
                self.at += 1;
                if self.request_errors {
                    if !escaped && *b < 32 {
                        return self.fail("Invalid control character at", self.at - 1);
                    }
                    if escaped && *b == b'u' {
                        if !self
                            .s
                            .as_bytes()
                            .get(self.at..self.at + 4)
                            .is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit))
                        {
                            return self.fail("Invalid \\uXXXX escape", self.at - 1);
                        }
                        self.at += 4;
                        escaped = false;
                        continue;
                    }
                    if escaped && !b"\"\\/bfnrt".contains(b) {
                        return self.fail("Invalid \\escape", self.at - 2);
                    }
                }
                if !escaped && *b == b'"' {
                    return serde_json::from_str(&self.s[start..self.at]).map_err(|e| {
                        crate::Error::Invalid(if self.request_errors {
                            "请求 JSON 包含 Rust 字符串无法表示的孤立 surrogate".into()
                        } else {
                            e.to_string()
                        })
                    });
                }
                escaped = !escaped && *b == b'\\';
            }
            self.fail("Unterminated string starting at", start)
        }
        fn value(&mut self, depth: usize) -> crate::Result<Json> {
            if depth > 1000 {
                return Err(crate::Error::Invalid("证据 JSON 递归过深".into()));
            }
            self.ws();
            let start = self.at;
            match self.s.as_bytes().get(self.at) {
                Some(b'"') => return self.string().map(Into::into),
                Some(b'{') => {
                    self.at += 1;
                    let mut out = Json::Object(Vec::new());
                    if self.take(b'}') {
                        return Ok(out);
                    }
                    loop {
                        let key = self.string()?;
                        if !self.take(b':') {
                            return self.fail("Expecting ':' delimiter", self.at);
                        }
                        out.set(key, self.value(depth + 1)?);
                        if self.take(b'}') {
                            return Ok(out);
                        }
                        if !self.take(b',') {
                            return self.fail("Expecting ',' delimiter", self.at);
                        }
                    }
                }
                Some(b'[') => {
                    self.at += 1;
                    let mut out = Vec::new();
                    if self.take(b']') {
                        return Ok(Json::Array(out));
                    }
                    loop {
                        out.push(self.value(depth + 1)?);
                        if self.take(b']') {
                            return Ok(Json::Array(out));
                        }
                        if !self.take(b',') {
                            return self.fail("Expecting ',' delimiter", self.at);
                        }
                    }
                }
                _ => {}
            }
            if self.request_errors {
                if let Some(word) = ["NaN", "Infinity", "-Infinity", "true", "false", "null"]
                    .into_iter()
                    .find(|w| self.s[self.at..].starts_with(w))
                {
                    self.at += word.len();
                } else if let Some(end) = self.request_number_end() {
                    self.at = end;
                } else {
                    return self.bad();
                }
            } else {
                while self.s.as_bytes().get(self.at).is_some_and(|b| !b" \t\r\n,]}".contains(b)) {
                    self.at += 1;
                }
            }
            let word = &self.s[start..self.at];
            match word {
                "NaN" => Ok(Json::Float(f64::NAN)),
                "Infinity" => Ok(Json::Float(f64::INFINITY)),
                "-Infinity" => Ok(Json::Float(f64::NEG_INFINITY)),
                "true" => Ok(true.into()),
                "false" => Ok(false.into()),
                "null" => Ok(None::<String>.into()),
                _ => {
                    // Preserve Python JSON number grammar without f64 coercion of large integers.
                    let bytes = word.as_bytes();
                    let mut at = usize::from(bytes.first() == Some(&b'-'));
                    if bytes.get(at) == Some(&b'0') {
                        at += 1;
                    } else {
                        let begin = at;
                        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                            at += 1;
                        }
                        if at == begin {
                            return self.bad();
                        }
                    }
                    let mut float = false;
                    if bytes.get(at) == Some(&b'.') {
                        float = true;
                        at += 1;
                        let begin = at;
                        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                            at += 1;
                        }
                        if at == begin {
                            return self.bad();
                        }
                    }
                    if bytes.get(at).is_some_and(|b| b"eE".contains(b)) {
                        float = true;
                        at += 1;
                        if bytes.get(at).is_some_and(|b| b"+-".contains(b)) {
                            at += 1;
                        }
                        let begin = at;
                        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                            at += 1;
                        }
                        if at == begin {
                            return self.bad();
                        }
                    }
                    if at != bytes.len() || word.is_empty() || word == "-" {
                        return self.bad();
                    }
                    if float {
                        word.parse::<f64>().map(Json::Float).map_err(|_| crate::Error::Invalid("无效 JSON 数字".into()))
                    } else if word.trim_start_matches('-').len() > 4300 {
                        if self.request_errors {
                            Err(crate::Error::Invalid(format!("Exceeds the limit (4300 digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit", word.trim_start_matches('-').len())))
                        } else {
                            self.bad()
                        }
                    } else {
                        Ok(Json::Integer(if word == "-0" { "0".into() } else { word.into() }))
                    }
                }
            }
        }
    }
    let mut parser = Parser { s: input, at: 0, request_errors };
    let v = parser.value(0)?;
    parser.ws();
    if parser.at == input.len() {
        Ok(v)
    } else {
        parser.fail("Extra data", parser.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_layout_is_separate_from_persisted_layout() {
        let mut v: Json = serde_json::from_str(
            r#"{"z":{"ja":"雪🙂","a":1.0},"a":"\u0000\b\t\n\f\r\u001f/\\\"","z":{"last":-0.0,"first":5.72e-5}}"#,
        )
        .unwrap();
        v.set("a", "\u{2028}\u{7f}".into());
        assert_eq!(v.dump(), "{\"z\": {\"last\": -0.0, \"first\": 5.72e-05}, \"a\": \"\u{2028}\u{7f}\"}");
        assert_eq!(crate::character_json::dump(&serde_json::json!({"z":1,"a":2}), false), "{\"a\": 2, \"z\": 1}");
    }
    #[test]
    fn python_evidence_numbers_and_nested_order() {
        let input = r#"{"z": NaN, "i": Infinity, "m": -Infinity, "big": 123456789012345678901234567890, "nested": {"z": false, "a": [null, -0, 1e400, 1.0]}}"#;
        assert_eq!(
            parse_python(input).unwrap().dump(),
            r#"{"z": NaN, "i": Infinity, "m": -Infinity, "big": 123456789012345678901234567890, "nested": {"z": false, "a": [null, 0, Infinity, 1.0]}}"#
        );
        assert_eq!(parse_python("2").unwrap().integer(), 2);
        assert_eq!(parse_python("-0").unwrap().integer(), 0);
        for invalid in ["+1", "01", "-.2", "1.", "[1,]", "{}x", "{\"a\":}", "1e", ""] {
            assert!(parse_python(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn request_errors_and_valid_values_match_python_reference() {
        let python = std::env::var("ILLUSTASH_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
        // 中文 Windows 上 Python 标准输出默认是 GBK；对照输出统一用 UTF-8。
        let output = std::process::Command::new(python).env("PYTHONUTF8", "1").args(["-c", r#"
import json, random
cases = ['', ' ', '{', '[', '{"x":}', '{"x":01}', '{"x":1.}', '{"x":1e+}', '{"x":tru}', '[1,]', '{"a":1,}', '{"a" 1}', '{"a":1 "b":2}', '{} xx', '{"x":"\\q"}', '{"x":"abc}', '{"x":"a\nb"}', '{"x":"\\u123"}', '{"x":"\\u12xx"}', '{"猫":"\\q"}', '{\n"猫":"\\q"}', '{"x":"\\ud800\\uxxxx"}', 'NaNx', 'truefalse', '-InfinityX', '-.2', '+1', '-']
cases += [json.dumps({'猫': '角色🙂', 'array': [False, None, 0.0, -0.0, 1e-7]}), '{"z":NaN,"i":Infinity,"m":-Infinity,"big":123456789012345678901234567890}', '9' * 4301]
r = random.Random(20261001)
for original in ['{"x":"角色\\n🙂","y":[1,2,true,null]}', '{"x":-12.34e-5}', '["a\\\\b",{},[]]']:
 for i in range(150):
  pos=r.randrange(len(original)); ch=r.choice('xyz012 ,:{}[]"\\\n猫')
  cases.append(original[:pos]+ch+original[pos+1:])
result=[]
for text in cases:
 try: result.append([text, True, json.dumps(json.loads(text),ensure_ascii=False)])
 except (ValueError, RecursionError) as e: result.append([text, False, str(e)])
print(json.dumps(result,ensure_ascii=False))
"#]).output().expect("Python JSON request reference");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let cases: Value = serde_json::from_slice(&output.stdout).unwrap();
        for case in cases.as_array().unwrap() {
            let text = case[0].as_str().unwrap();
            let expected = case[2].as_str().unwrap();
            if case[1].as_bool().unwrap() {
                assert_eq!(parse_request(text).unwrap().dump(), expected, "{text:?}");
            } else {
                assert!(
                    matches!(parse_request(text), Err(crate::Error::Invalid(message)) if message == expected),
                    "{text:?}: Rust={:?}, Python={expected}",
                    parse_request(text)
                );
            }
        }
    }
    #[test]
    fn request_mode_does_not_change_evidence_errors_or_silently_replace_surrogates() {
        for text in ["01", "1e", "truefalse", "{}x", "{", "[1,]", "{\"x\":}"] {
            assert!(matches!(parse_python(text), Err(crate::Error::Invalid(message)) if message == "无效 JSON 证据"));
        }
        assert!(
            matches!(parse_request(r#""\ud800""#), Err(crate::Error::Invalid(message)) if message.contains("孤立 surrogate"))
        );
        assert_eq!(parse_request(r#""\ud83d\ude42""#).unwrap().text(), "🙂");
    }
    #[test]
    fn float_and_string_bytes_match_python_not_just_semantic_json() {
        let python = std::env::var("ILLUSTASH_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
        let result = std::process::Command::new(python)
            .env("PYTHONUTF8", "1")
            .args([
                "-c",
                r#"
import random,struct,json,math
r=random.Random(20260930)
v=[0.0,-0.0,1e-4,1e-5,1e15,1e16,1e20,1e21,1.2345678901234567,5e-324,1.7976931348623157e308]
for i in range(20000):
 f=struct.unpack('d',r.randbytes(8))[0]
 if math.isfinite(f):v.append(f)
v += [chr(i) for i in range(128)] + ['雪🙂', '\u2028\u2029', '\\"/']
print(json.dumps({'input':v,'expected':json.dumps(v,ensure_ascii=False)},ensure_ascii=False))
"#,
            ])
            .output()
            .expect("Python JSON reference");
        assert!(result.status.success());
        let reference: Json = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(reference.get("input").unwrap().dump(), reference.get("expected").unwrap().text());
        let parsed = parse_python(std::str::from_utf8(&result.stdout).unwrap()).unwrap();
        assert_eq!(parsed.get("input").unwrap().dump(), reference.get("expected").unwrap().text());
    }
}
