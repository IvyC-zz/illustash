"""溯源模块：找回画师（docs/PROVENANCE.md）。

独立于核心：只写图库包里的 provenance.sqlite，对 source.sqlite、library.sqlite 只读。以后核心迁到 Rust，这里可以一直用 Python。
两层结构：种子来源（sources/）产出候选画师或直接命中的原帖，本地验证（verify）拉公开作品小图逐张比对，比对上才算确认。
"""
