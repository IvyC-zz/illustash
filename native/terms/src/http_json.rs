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
    /// A field the query or the code building this object always provides; a missing one is a bug.
    #[track_caller]
    pub(crate) fn field(&self, key: &str) -> &Json {
        match self.get(key) {
            Some(v) => v,
            None => panic!("HTTP object has no field {key}"),
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
            // Python writes NaN/Infinity, which JSON.parse refuses; evidence may still hold them (#265).
            Self::Float(v) if !v.is_finite() => out.extend_from_slice(b"null"),
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
    parse(input, false).map_err(|(_, e)| e)
}
/// Request errors follow CPython JSONDecodeError; evidence callers retain the original refusal behavior.
pub(crate) fn parse_request(input: &str) -> crate::Result<Json> {
    parse(input, true).map_err(|(_, e)| e)
}
/// Why evidence JSON was refused, for callers that treat some refusals differently (#204); the error keeps the words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Malformed,
    /// A string escapes half of a UTF-16 surrogate pair.
    LoneSurrogate,
    TooDeep,
}
/// `parse_python`, saying which kind of refusal a failure is.
pub(crate) fn parse_python_refusal(input: &str) -> std::result::Result<Json, (Refusal, crate::Error)> {
    parse(input, false)
}
fn parse(input: &str, request_errors: bool) -> std::result::Result<Json, (Refusal, crate::Error)> {
    let mut parser = Parser { s: input, at: 0, request_errors, refusal: Refusal::Malformed };
    let v = parser.value(0).and_then(|v| {
        parser.ws();
        if parser.at == input.len() {
            Ok(v)
        } else {
            parser.fail("Extra data", parser.at)
        }
    });
    v.map_err(|e| (parser.refusal, e))
}

struct Parser<'a> {
    s: &'a str,
    at: usize,
    request_errors: bool,
    refusal: Refusal,
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
        Err(crate::Error::Invalid(format!("{message}: line {line} column {column} (char {})", before.chars().count())))
    }
    // CPython's scanner consumes a valid numeric prefix, leaving punctuation errors to the container.
    fn request_number_end(&self) -> Option<usize> {
        let bytes = self.s.as_bytes();
        let mut at = self.at + usize::from(bytes.get(self.at) == Some(&b'-'));
        match bytes.get(at) {
            Some(b'0') => at += 1,
            Some(b'1'..=b'9') => at = digits_end(bytes, at),
            _ => return None,
        }
        if bytes.get(at) == Some(&b'.') && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
            at = digits_end(bytes, at + 2);
        }
        if bytes.get(at).is_some_and(|b| b"eE".contains(b)) {
            let mut end = at + 1;
            if bytes.get(end).is_some_and(|b| b"+-".contains(b)) {
                end += 1;
            }
            if bytes.get(end).is_some_and(u8::is_ascii_digit) {
                at = digits_end(bytes, end);
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
                    let hex = self.s.as_bytes().get(self.at..self.at + 4);
                    if !hex.is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit)) {
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
                return self.decoded(start);
            }
            escaped = !escaped && *b == b'\\';
        }
        self.fail("Unterminated string starting at", start)
    }
    /// The complete string literal from `start`, decoded by serde_json.
    fn decoded(&mut self, start: usize) -> crate::Result<String> {
        serde_json::from_str(&self.s[start..self.at]).map_err(|e| {
            let e = e.to_string();
            // serde_json has no public error codes: its own (English) message names a lone surrogate.
            if self.request_errors || e.starts_with("lone leading surrogate") {
                self.refusal = Refusal::LoneSurrogate;
            }
            crate::Error::Invalid(if self.request_errors {
                "请求 JSON 包含 Rust 字符串无法表示的孤立 surrogate".into()
            } else {
                e
            })
        })
    }
    fn value(&mut self, depth: usize) -> crate::Result<Json> {
        if depth > 1000 {
            self.refusal = Refusal::TooDeep;
            return Err(crate::Error::Invalid("证据 JSON 递归过深".into()));
        }
        self.ws();
        let start = self.at;
        match self.s.as_bytes().get(self.at) {
            Some(b'"') => return self.string().map(Into::into),
            Some(b'{') => return self.object(depth),
            Some(b'[') => return self.array(depth),
            _ => {}
        }
        self.word()?;
        let s = self.s;
        match &s[start..self.at] {
            "NaN" => Ok(Json::Float(f64::NAN)),
            "Infinity" => Ok(Json::Float(f64::INFINITY)),
            "-Infinity" => Ok(Json::Float(f64::NEG_INFINITY)),
            "true" => Ok(true.into()),
            "false" => Ok(false.into()),
            "null" => Ok(None::<String>.into()),
            word => self.number(word),
        }
    }
    fn object(&mut self, depth: usize) -> crate::Result<Json> {
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
    fn array(&mut self, depth: usize) -> crate::Result<Json> {
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
    /// Moves past a constant or number: CPython's scanner for requests, up to a delimiter for evidence.
    fn word(&mut self) -> crate::Result<()> {
        if !self.request_errors {
            while self.s.as_bytes().get(self.at).is_some_and(|b| !b" \t\r\n,]}".contains(b)) {
                self.at += 1;
            }
        } else if let Some(word) = ["NaN", "Infinity", "-Infinity", "true", "false", "null"]
            .into_iter()
            .find(|w| self.s[self.at..].starts_with(w))
        {
            self.at += word.len();
        } else if let Some(end) = self.request_number_end() {
            self.at = end;
        } else {
            return self.bad();
        }
        Ok(())
    }
    /// Python JSON number grammar, without f64 coercion of large integers.
    fn number(&self, word: &str) -> crate::Result<Json> {
        let bytes = word.as_bytes();
        let mut at = usize::from(bytes.first() == Some(&b'-'));
        if bytes.get(at) == Some(&b'0') {
            at += 1;
        } else {
            let first = at;
            at = digits_end(bytes, at);
            if at == first {
                return self.bad();
            }
        }
        let mut float = false;
        if bytes.get(at) == Some(&b'.') {
            float = true;
            let first = at + 1;
            at = digits_end(bytes, first);
            if at == first {
                return self.bad();
            }
        }
        if bytes.get(at).is_some_and(|b| b"eE".contains(b)) {
            float = true;
            at += 1;
            if bytes.get(at).is_some_and(|b| b"+-".contains(b)) {
                at += 1;
            }
            let first = at;
            at = digits_end(bytes, at);
            if at == first {
                return self.bad();
            }
        }
        if at != bytes.len() || word.is_empty() || word == "-" {
            return self.bad();
        }
        let digits = word.trim_start_matches('-').len();
        if float {
            word.parse::<f64>().map(Json::Float).map_err(|_| crate::Error::Invalid("无效 JSON 数字".into()))
        } else if digits <= 4300 {
            Ok(Json::Integer(if word == "-0" { "0".into() } else { word.into() }))
        } else if self.request_errors {
            Err(crate::Error::Invalid(format!(
                "Exceeds the limit (4300 digits) for integer string conversion: value has {digits} digits; \
                 use sys.set_int_max_str_digits() to increase the limit"
            )))
        } else {
            self.bad()
        }
    }
}
/// Where the run of ASCII digits starting at `at` ends.
fn digits_end(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    at
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
        let input = concat!(
            r#"{"z": NaN, "i": Infinity, "m": -Infinity, "big": 123456789012345678901234567890, "#,
            r#""nested": {"z": false, "a": [null, -0, 1e400, 1.0]}}"#
        );
        let dumped = parse_python(input).unwrap().dump();
        assert_eq!(
            dumped,
            concat!(
                r#"{"z": null, "i": null, "m": null, "big": 123456789012345678901234567890, "#,
                r#""nested": {"z": false, "a": [null, 0, null, 1.0]}}"#
            )
        );
        // A strict parser accepts the reply; the non-finite values are null in it.
        let strict: Value = serde_json::from_str(&dumped).unwrap();
        for pointer in ["/z", "/i", "/m", "/nested/a/2"] {
            assert_eq!(strict.pointer(pointer), Some(&Value::Null), "{pointer}");
        }
        assert_eq!(Json::Array(vec![Json::Float(f64::NAN), Json::Float(f64::NEG_INFINITY)]).dump(), "[null, null]");
        assert_eq!(parse_python("2").unwrap().integer(), 2);
        assert_eq!(parse_python("-0").unwrap().integer(), 0);
        for invalid in ["+1", "01", "-.2", "1.", "[1,]", "{}x", "{\"a\":}", "1e", ""] {
            assert!(parse_python(invalid).is_err(), "{invalid}");
        }
    }
    /// Python `json` outputs recorded once (#9) by the scripts that used to run here each time
    /// (fixed seeds; CPython 3.12): `tests/fixtures/json/`.
    fn recorded(name: &str) -> Vec<u8> {
        use std::io::Read;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/json").join(name);
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap()).read_to_end(&mut out).unwrap();
        out
    }
    #[test]
    fn request_errors_and_valid_values_match_python_reference() {
        // Python's json.loads/json.dumps(ensure_ascii=False) on malformed, edge and randomly mutated
        // texts: [text, parsed, re-dumped text or error message].
        let cases: Value = serde_json::from_slice(&recorded("request_cases.json.gz")).unwrap();
        let mut non_finite = 0;
        for case in cases.as_array().unwrap() {
            let text = case[0].as_str().unwrap();
            let mut expected = case[2].as_str().unwrap().to_owned();
            if case[1].as_bool().unwrap() {
                // Python re-dumps NaN/Infinity as is; HTTP replies write null (#265).
                if expected.contains("NaN") || expected.contains("Infinity") {
                    non_finite += 1;
                    expected = expected.replace("-Infinity", "null").replace("Infinity", "null").replace("NaN", "null");
                }
                assert_eq!(parse_request(text).unwrap().dump(), expected, "{text:?}");
            } else {
                assert!(
                    matches!(parse_request(text), Err(crate::Error::Invalid(message)) if message == expected),
                    "{text:?}: Rust={:?}, Python={expected}",
                    parse_request(text)
                );
            }
        }
        // Only the one case written for non-finite constants; a string mentioning NaN would need its own handling.
        assert_eq!(non_finite, 1);
    }
    #[test]
    fn request_mode_does_not_change_evidence_errors_or_silently_replace_surrogates() {
        for text in ["01", "1e", "truefalse", "{}x", "{", "[1,]", "{\"x\":}"] {
            assert!(matches!(parse_python(text), Err(crate::Error::Invalid(message)) if message == "无效 JSON 证据"));
        }
        let lone = parse_request(r#""\ud800""#);
        assert!(matches!(lone, Err(crate::Error::Invalid(message)) if message.contains("孤立 surrogate")));
        assert_eq!(parse_request(r#""\ud83d\ude42""#).unwrap().text(), "🙂");
    }
    #[test]
    fn refusals_say_which_kind_without_the_words() {
        let kind = |text: &str| parse_python_refusal(text).map(|_| ()).map_err(|(kind, _)| kind);
        // A trailing half alone, or a leading half followed by another escape: serde_json calls both lone surrogates.
        assert_eq!(kind(r#"{"a": "\udc00"}"#), Err(Refusal::LoneSurrogate));
        assert_eq!(kind(r#""\ud800\u0041""#), Err(Refusal::LoneSurrogate));
        // A leading half followed by plain text is an unfinished escape to serde_json, as before #204.
        assert_eq!(kind(r#""\ud800x""#), Err(Refusal::Malformed));
        assert_eq!(kind(r#""\x""#), Err(Refusal::Malformed));
        for text in ["{", "1.5e999x", "[1,]"] {
            assert_eq!(kind(text), Err(Refusal::Malformed), "{text}");
        }
        // A thousand levels need more than a test thread's stack in a debug build.
        let deep = std::thread::Builder::new().stack_size(256 << 20).spawn(move || {
            assert_eq!(kind(&format!("{}{}", "[".repeat(1001), "]".repeat(1001))), Ok(()));
            parse_python_refusal(&"[".repeat(1002)).unwrap_err()
        });
        let (refusal, error) = deep.unwrap().join().unwrap();
        assert_eq!((refusal, error.to_string().as_str()), (Refusal::TooDeep, "不合法：证据 JSON 递归过深"));
    }
    #[test]
    fn float_and_string_bytes_match_python_not_just_semantic_json() {
        // json.dumps(ensure_ascii=False) of 20,000 random finite doubles, float boundaries, ASCII and
        // separators: both parsers must reproduce Python's bytes.
        let expected = String::from_utf8(recorded("float_bytes.json.gz")).unwrap();
        let reference: Json = serde_json::from_str(&expected).unwrap();
        assert_eq!(reference.dump(), expected);
        assert_eq!(parse_python(&expected).unwrap().dump(), expected);
    }
}
