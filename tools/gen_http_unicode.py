"""用应用所用的 Python（目前是 Unicode 15.0）生成与 Python 一致的共用 Unicode 表。

HTTP 整数解析和共用的错误 repr 用这些表；名字的 casefold 和已存的 JSON 不受影响。
运行：.venv/bin/python tools/gen_http_unicode.py
"""
from pathlib import Path
import unicodedata


def main():
    zeroes = []
    ranges = []
    start = None
    for i in range(0x110000):
        ch = chr(i)
        try:
            if unicodedata.decimal(ch) == 0:
                zeroes.append(i)
        except ValueError:
            pass
        if not ch.isprintable():
            if start is None:
                start = i
        elif start is not None:
            ranges.append((start, i - 1)); start = None
    if start is not None:
        ranges.append((start, 0x10ffff))
    out = Path(__file__).resolve().parents[1] / 'crates/core/src/http_unicode.rs'
    lines = [f'//! Python int/repr 的共用表（沿用历史文件名），由 tools/gen_http_unicode.py 生成（Unicode {unicodedata.unidata_version}）。',
             '#[rustfmt::skip]', 'const ZEROES: &[u32] = &[']
    lines += ['    '+','.join(hex(i) for i in zeroes[j:j+12])+',' for j in range(0,len(zeroes),12)]
    lines += ['];', '#[rustfmt::skip]', 'const NONPRINTABLE: &[(u32,u32)] = &[']
    lines += ['    '+','.join(f'({a:#x},{b:#x})' for a,b in ranges[j:j+8])+',' for j in range(0,len(ranges),8)]
    lines += ['];', '''pub fn decimal(c:char)->Option<u8>{
    let n=c as u32; ZEROES.iter().find(|s|n>=**s && n<**s+10).map(|s|(n-s) as u8)
}
pub fn printable(c:char)->bool{
    let n=c as u32; let i=NONPRINTABLE.partition_point(|(_,end)|*end<n);
    NONPRINTABLE.get(i).is_none_or(|(start,_)|n<*start)
}''']
    out.write_text('\n'.join(lines)+'\n')


if __name__ == '__main__':
    main()
