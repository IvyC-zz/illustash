//! 业务适配器和传输适配器共用的文本规则：空白集和 repr 式引号转义，与已存数据和既有响应一致（沿用旧实现）。
pub fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
pub fn trim(s: &str) -> &str {
    s.trim_matches(whitespace)
}
pub fn python_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = quote.to_string();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c)
            }
            c if !crate::compat_unicode::printable(c) => {
                let n = c as u32;
                out.push_str(&if n <= 255 {
                    format!("\\x{n:02x}")
                } else if n <= 65535 {
                    format!("\\u{n:04x}")
                } else {
                    format!("\\U{n:08x}")
                });
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_text_retains_python_unicode_quotes_and_whitespace() {
        assert_eq!(trim("\u{1c} 雪\u{1f}"), "雪");
        assert_eq!(python_repr("a'b"), "\"a'b\"");
        assert_eq!(python_repr("\\\n\r\t\0\u{a0}\u{200b}\u{e000}雪😀"), "'\\\\\\n\\r\\t\\x00\\xa0\\u200b\\ue000雪😀'");
        assert_eq!(python_repr("\u{f0000}"), "'\\U000f0000'");
    }
}
