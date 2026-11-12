"""生成 crates/core/src/casefold.rs：把 Python 的 str.casefold() 做成一张表，供核心的搜索使用（names_search.rs）。

str.casefold() 逐个码位折叠（Unicode 完全大小写折叠，不看上下文），所以列出它会改变的每个码位的表就能精确重现它。
为 App 提供服务的 Python 的 Unicode 版本变了时，用那个 Python 重新运行：

    python tools/gen_casefold.py
"""
from pathlib import Path
import sys
import unicodedata

OUT = Path(__file__).resolve().parent.parent / 'crates' / 'core' / 'src' / 'casefold.rs'


def main():
    rows = [(i, chr(i).casefold()) for i in range(0x110000)
            if not 0xD800 <= i <= 0xDFFF and chr(i).casefold() != chr(i)]
    lines = [
        f'//! 由 tools/gen_casefold.py 生成（Python {sys.version.split()[0]}，Unicode {unicodedata.unidata_version}），不要手改。',
        '//! Python 的 `str.casefold()`：逐个码位做完全大小写折叠，表里是所有会变的码位。',
        '',
        '#[rustfmt::skip]',
        f'static FOLD: [(char, &str); {len(rows)}] = [',
    ]
    for i, folded in rows:
        escaped = ''.join(f'\\u{{{ord(ch):x}}}' for ch in folded)
        lines.append(f"    ('\\u{{{i:x}}}', \"{escaped}\"),")
    lines += [
        '];',
        '',
        '/// `str.casefold()`。',
        'pub fn casefold(s: &str) -> String {',
        '    let mut out = String::with_capacity(s.len());',
        '    for ch in s.chars() {',
        '        match FOLD.binary_search_by_key(&ch, |(c, _)| *c) {',
        '            Ok(i) => out.push_str(FOLD[i].1),',
        '            Err(_) => out.push(ch),',
        '        }',
        '    }',
        '    out',
        '}',
        '',
    ]
    OUT.write_text('\n'.join(lines), encoding='utf-8')
    print(f'{OUT}: {len(rows)} code points')


if __name__ == '__main__':
    main()
