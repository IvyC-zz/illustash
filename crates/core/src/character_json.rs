//! 已持久化的 Python json.dumps 格式（身份指纹和快照 JSON）。
//! 对象按键排序，这是 identity.propose 的要求；图指纹里只有数组、字符串和 null。图里的数字从不走浮点分支。
use serde_json::Value;

pub fn dump(value: &Value, ascii: bool) -> String {
    match value {
        Value::String(s) => {
            let encoded = serde_json::to_string(s).expect("JSON string");
            if !ascii {
                return encoded;
            }
            let mut out = String::new();
            for ch in encoded.chars() {
                if ch as u32 > 126 {
                    for unit in ch.encode_utf16(&mut [0; 2]) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                } else {
                    out.push(ch);
                }
            }
            out
        }
        Value::Array(values) => format!("[{}]", values.iter().map(|v| dump(v, ascii)).collect::<Vec<_>>().join(", ")),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(k, v)| format!("{}: {}", dump(&Value::String(k.clone()), ascii), dump(v, ascii)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(n) if n.is_f64() => {
            let mut s = n.to_string();
            let f = n.as_f64().expect("float");
            if f != 0.0 && f.abs() < 1e-4 && !s.contains('e') {
                let sign = if f < 0.0 { "-" } else { "" };
                let decimal = s.trim_start_matches('-').trim_start_matches("0.");
                let zeroes = decimal.bytes().take_while(|b| *b == b'0').count();
                let digits = decimal[zeroes..].trim_end_matches('0');
                let mantissa =
                    if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits.to_owned() };
                s = format!("{sign}{mantissa}e-{}", zeroes + 1);
            }
            if let Some((mantissa, exponent)) = s.split_once('e') {
                let exponent: i32 = exponent.parse().expect("JSON exponent");
                format!("{mantissa}e{exponent:+03}")
            } else {
                s
            }
        }
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn python_ascii_spaces_and_float_boundaries() {
        assert_eq!(
            dump(&json!(["人物\u{7f}🙂", null, ["b", "a"]]), true),
            r#"["\u4eba\u7269\u007f\ud83d\ude42", null, ["b", "a"]]"#
        );
        assert_eq!(dump(&json!([1e-7,5.72e-5,1e-5,0.1,123.0,1e15,1e16,1e21,1.5e22,-0.0,2.099447491238697e-9]),false),
            "[1e-07, 5.72e-05, 1e-05, 0.1, 123.0, 1000000000000000.0, 1e+16, 1e+21, 1.5e+22, -0.0, 2.099447491238697e-09]");
        assert_eq!(dump(&json!({"z":true,"a":"人物"}), false), "{\"a\": \"人物\", \"z\": true}");
    }
}
