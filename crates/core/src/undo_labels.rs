//! 撤销栈里按张数写的几种提示：写（`character_commands`）和读（native/terms `http_app.rs` 把提示里的角色换成现在的名字）
//! 共用这一份句式，改措辞时两边一起变（#204）。
//!
//! 读是在解析已存的数据格式，不是一般的文字判断：这些步骤的撤销载荷里只有快照，没有操作种类和张数，张数只记在提示里。
//! #201 在撤销载荷里存下操作种类和张数之后，`count` 和读的那一边可以删掉。
use crate::compat_unicode::decimal;

/// 一种提示句式。占位：`{n}` 张数，`{name}` 角色，`{other}` 换成的角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoLabel {
    Set,
    Remove,
    Add,
    Drop,
    Swap,
    Sweep,
}

impl UndoLabel {
    pub const ALL: [UndoLabel; 6] =
        [UndoLabel::Set, UndoLabel::Remove, UndoLabel::Add, UndoLabel::Drop, UndoLabel::Swap, UndoLabel::Sweep];

    pub fn template(self) -> &'static str {
        match self {
            UndoLabel::Set => "已将 {n} 张设为 {name}",
            UndoLabel::Remove => "已将 {n} 张移出 {name}",
            UndoLabel::Add => "已给 {n} 张加上 {name}",
            UndoLabel::Drop => "已从 {n} 张去掉 {name}",
            UndoLabel::Swap => "已将 {n} 张从 {name} 换成 {other}",
            UndoLabel::Sweep => "已将 {name} 标为已检查（{n} 张）",
        }
    }

    /// 填好占位的提示。填进去的文字不再当作占位读。
    pub fn fill(self, n: &str, name: &str, other: &str) -> String {
        let mut out = String::new();
        let mut rest = self.template();
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let end = start + rest[start..].find('}').expect("placeholder is closed");
            out.push_str(match &rest[start..=end] {
                "{n}" => n,
                "{name}" => name,
                "{other}" => other,
                _ => unreachable!("unknown placeholder"),
            });
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// 已存的提示是这种句式时，它记的张数（原样，可以是任意 Unicode 十进制数字）。
    ///
    /// 张数在开头的句式：开头的字之后紧跟张数和下一段字，其余各段在张数之后出现即可；角色名本身什么都可以是。
    /// 张数在末尾的句式（`Sweep`）：张数之前的那段字取最后一次出现。
    pub fn count(self, label: &str) -> Option<&str> {
        let (head, tail) = self.template().split_once("{n}").expect("template has a count");
        let head = pieces(head);
        let tail = pieces(tail);
        let s = label.strip_prefix(head[0])?;
        if head.len() == 1 {
            let (n, rest) = digits(s);
            let follows = rest.starts_with(tail[0]) && tail[1..].iter().all(|piece| rest.contains(piece));
            (!n.is_empty() && follows).then_some(n)
        } else {
            let (_, after) = s.rsplit_once(head[head.len() - 1])?;
            let n = after.strip_suffix(tail[tail.len() - 1])?;
            (!n.is_empty() && digits(n).1.is_empty()).then_some(n)
        }
    }
}

/// 句式里占位之间的字，没有字的段不算。
fn pieces(template: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push(&rest[..start]);
        rest = &rest[start + rest[start..].find('}').expect("placeholder is closed") + 1..];
    }
    out.push(rest);
    let mut out: Vec<&str> = out.into_iter().filter(|piece| !piece.is_empty()).collect();
    if out.is_empty() {
        out.push("");
    }
    out
}

/// 开头的一串十进制数字和其余部分。
fn digits(s: &str) -> (&str, &str) {
    let n = s.char_indices().find(|(_, c)| decimal(*c).is_none()).map_or(s.len(), |(i, _)| i);
    s.split_at(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_written_reads_back_as_its_kind_and_count() {
        for kind in UndoLabel::ALL {
            for (n, name, other) in [("2", "雪", "月"), ("10", "", ""), ("３", "a {n} 张从 b", "{name}")] {
                let label = kind.fill(n, name, other);
                assert_eq!(kind.count(&label), Some(n), "{label}");
                for others in UndoLabel::ALL.into_iter().filter(|k| *k != kind) {
                    assert_eq!(others.count(&label), None, "{others:?} read {label}");
                }
            }
        }
    }

    #[test]
    fn labels_are_the_stored_bytes() {
        assert_eq!(UndoLabel::Swap.fill("2", "old", "new"), "已将 2 张从 old 换成 new");
        assert_eq!(UndoLabel::Sweep.fill("2", "stale", ""), "已将 stale 标为已检查（2 张）");
        assert_eq!(UndoLabel::Drop.fill("1", "x", "ignored"), "已从 1 张去掉 x");
    }

    #[test]
    fn counts_need_digits_where_the_template_has_them() {
        assert_eq!(UndoLabel::Set.count("已将 张设为 x"), None);
        assert_eq!(UndoLabel::Set.count("已将 2x 张设为 x"), None);
        assert_eq!(UndoLabel::Swap.count("已将 2 张从 old 换成"), None);
        assert_eq!(UndoLabel::Sweep.count("已将 a 标为已检查（2 张） 标为已检查（3 张）"), Some("3"));
        assert_eq!(UndoLabel::Sweep.count("已将 a 标为已检查（ 张）"), None);
        assert_eq!(UndoLabel::Sweep.count("已将 a 标为已检查（2a 张）"), None);
        assert_eq!(UndoLabel::Set.count("literal-history"), None);
    }
}
