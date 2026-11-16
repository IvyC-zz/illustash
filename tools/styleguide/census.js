// 由 tools/styleguide/census.py 从 web/static/app.css 生成，不要手改。
window.CENSUS = {
 "source": "web/static/app.css",
 "sha256": "4cb475c1a06174647dcc34f2bce7e95e65866cfa074e3278b6b1c28b5e728b12",
 "lines": 1504,
 "rules": 1347,
 "themeVars": {
  "light": {
   "--canvas": "#f4f4f6",
   "--sheet": "#fff",
   "--ink": "#1b1b1f",
   "--ink2": "#46464e",
   "--muted": "#8b8b94",
   "--faint": "#b8b8c0",
   "--line": "#e8e8ec",
   "--hair": "rgba(20,20,35,.08)",
   "--soft": "#f2f2f5",
   "--accent": "#7c80d8",
   "--accent-btn": "#6a6ecf",
   "--accent-soft": "#efeffb",
   "--accent-ink": "#4f52b0",
   "--warn": "#e08a2c",
   "--r18": "#e0466e",
   "--r18-soft": "#fdeaf0",
   "--sh": "20,20,35",
   "--mono": "ui-monospace,\"SF Mono\",SFMono-Regular,Menlo,monospace",
   "--ease": "cubic-bezier(.2,.8,.2,1)",
   "--sheet-shadow": "0 0 0 1px var(--hair),0 1px 2px rgba(var(--sh),.03),0 12px 32px -16px rgba(var(--sh),.16)",
   "--tone-warn-bg": "#fdf3e7",
   "--tone-warn-ink": "#8a5a22",
   "--tone-sand-bg": "#f6ecd8",
   "--tone-sand-ink": "#7a6a3e",
   "--tone-amber-bg": "#fbf1de",
   "--tone-amber-ink": "#9a6b1f",
   "--tone-steel-bg": "#eaf0f8",
   "--tone-steel-ink": "#4a6fa5",
   "--tone-indigo-bg": "#dedff1",
   "--tone-indigo-ink": "#2d3073",
   "--tone-blue-bg": "#e1ebf6",
   "--tone-blue-ink": "#2f5f93",
   "--tone-lilac-bg": "#f1ecf4",
   "--tone-lilac-ink": "var(--muted)",
   "--tone-purple-bg": "#efe4f3",
   "--tone-purple-ink": "#744a86",
   "--tone-clay-bg": "#f7e9dc",
   "--tone-clay-ink": "#98572a",
   "--atag-open-bg": "#eef0fbf2",
   "--atag-open-ink": "#3a3f86",
   "--atag-open-ring": "#6f76c966",
   "--atag-yes-bg": "#e9f6eef2",
   "--atag-yes-ink": "#2f7a4f",
   "--atag-no-bg": "#fbeceaf2",
   "--atag-no-ink": "#a8453d",
   "--hint-bg": "#fffffff0",
   "--hint-ink": "#282b64",
   "--hint-ring": "#282b6455",
   "--hint-bg-hover": "#282b64",
   "--glass-chip": "rgba(255,255,255,.8)",
   "--glass-pin": "color-mix(in srgb,var(--sheet) 85%,transparent)",
   "--glass-head": "rgba(255,255,255,.88)",
   "--bar-bg": "rgba(24,24,30,.86)",
   "--toast-bg": "rgba(24,24,30,.88)",
   "--menu-group": "var(--sheet)",
   "--splash-bg": "radial-gradient(120% 90% at 50% 42%,#fff 0%,#f6f5fd 55%,#ecebf9 100%)",
   "--switch-off": "#d4d4da",
   "--view-sw-off": "rgba(var(--sh),.14)",
   "--seg-on": "var(--sheet)",
   "--wash-hover": "rgba(var(--sh),.03)",
   "--chip-hover": "#e0e0e5",
   "--palette-hover": "#e2e2e7",
   "--btn-hover-filter": "brightness(.96)",
   "--badge-bg": "var(--soft)",
   "--badge-ink": "var(--ink2)",
   "--tag-on-ink": "var(--accent)",
   "--scroll-thumb": "#d7d7de",
   "--select-arrow": "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20'%3E%3Cpath d='m6 8 4 4 4-4' fill='none' stroke='%238b8b94' stroke-width='1.6' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
   "--r18-on": "#fff",
   "--ok": "#34c38f",
   "--bad": "#d9534f",
   "--err": "#b5523b"
  },
  "dark": {
   "--tour-dim": "rgba(0,0,0,.66)",
   "--canvas": "#1d1d21",
   "--sheet": "#28282d",
   "--ink": "#eeeef2",
   "--ink2": "#c8c8d0",
   "--muted": "#9a9aa3",
   "--faint": "#6d6d76",
   "--line": "#3a3a41",
   "--hair": "rgba(255,255,255,.09)",
   "--soft": "#323238",
   "--accent": "#a1a4f3",
   "--accent-btn": "#8286e6",
   "--accent-soft": "#35375f",
   "--accent-ink": "#c0c2ff",
   "--warn": "#f0a04b",
   "--r18": "#ff6b8f",
   "--r18-soft": "#48262f",
   "--sh": "0,0,0",
   "--sheet-shadow": "0 0 0 1px var(--hair),0 12px 32px -16px rgba(0,0,0,.45)",
   "--tone-warn-bg": "#35271a",
   "--tone-warn-ink": "#f0c68a",
   "--tone-sand-bg": "#3a2f18",
   "--tone-sand-ink": "#eac27a",
   "--tone-amber-bg": "#3a2f18",
   "--tone-amber-ink": "#eac27a",
   "--tone-steel-bg": "#1c2a40",
   "--tone-steel-ink": "#8dbbff",
   "--tone-indigo-bg": "#25264a",
   "--tone-indigo-ink": "#b9bbff",
   "--tone-blue-bg": "#1c2a40",
   "--tone-blue-ink": "#8dbbff",
   "--tone-lilac-bg": "#2b2433",
   "--tone-lilac-ink": "#d0b4dd",
   "--tone-purple-bg": "#2d2236",
   "--tone-purple-ink": "#c9a0dc",
   "--tone-clay-bg": "#36271a",
   "--tone-clay-ink": "#eab28a",
   "--atag-open-bg": "rgba(70,74,130,.85)",
   "--atag-open-ink": "#e4e6ff",
   "--atag-open-ring": "#8f95e066",
   "--atag-yes-bg": "rgba(40,90,62,.85)",
   "--atag-yes-ink": "#d4f2df",
   "--atag-no-bg": "rgba(110,46,42,.85)",
   "--atag-no-ink": "#f6d8d4",
   "--hint-bg": "rgba(46,46,52,.9)",
   "--hint-ink": "var(--accent-ink)",
   "--hint-ring": "var(--accent)",
   "--hint-bg-hover": "var(--accent-btn)",
   "--glass-chip": "rgba(46,46,52,.82)",
   "--glass-pin": "rgba(46,46,52,.85)",
   "--glass-head": "rgba(40,40,45,.88)",
   "--bar-bg": "rgba(58,58,66,.9)",
   "--toast-bg": "rgba(58,58,66,.94)",
   "--menu-group": "#34343a",
   "--splash-bg": "radial-gradient(120% 90% at 50% 42%,#2b2b33 0%,#222227 55%,#1b1b1f 100%)",
   "--switch-off": "#46464e",
   "--view-sw-off": "rgba(255,255,255,.16)",
   "--seg-on": "#42424a",
   "--wash-hover": "rgba(255,255,255,.04)",
   "--chip-hover": "var(--soft)",
   "--palette-hover": "var(--soft)",
   "--btn-hover-filter": "brightness(1.12)",
   "--badge-bg": "var(--accent-soft)",
   "--badge-ink": "var(--accent-ink)",
   "--tag-on-ink": "var(--accent-ink)",
   "--scroll-thumb": "#4a4a52",
   "--select-arrow": "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20'%3E%3Cpath d='m6 8 4 4 4-4' fill='none' stroke='%238d8d96' stroke-width='1.6' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
   "--r18-on": "#2a0f17"
  },
  "uses": {
   "--ink": 53,
   "--canvas": 11,
   "--fs-13": 30,
   "--hair": 34,
   "--sh": 37,
   "--muted": 138,
   "--sheet": 67,
   "--soft": 87,
   "--ink2": 73,
   "--accent": 43,
   "--accent-ink": 47,
   "--accent-btn": 13,
   "--accent-soft": 36,
   "--fs-10": 14,
   "--line": 72,
   "--radius-4": 20,
   "--space-4": 95,
   "--space-10": 97,
   "--space-12": 74,
   "--space-8": 133,
   "--radius-6": 34,
   "--h-36": 16,
   "--fs-15": 10,
   "--fs-11": 109,
   "--fs-12": 136,
   "--space-2": 49,
   "--space-14": 44,
   "--space-6": 116,
   "--radius-round": 16,
   "--h-32": 12,
   "--fs-16": 2,
   "--gutter": 13,
   "--space-16": 27,
   "--radius-pill": 48,
   "--fs-22": 3,
   "--fs-20": 4,
   "--tone-sand-ink": 3,
   "--radius-8": 17,
   "--faint": 24,
   "--z-popover": 1,
   "--scroll-thumb": 2,
   "--ease": 46,
   "--z-page-2": 6,
   "--glass-chip": 2,
   "--space-1": 26,
   "--z-page-4": 2,
   "--tone-warn-bg": 2,
   "--tone-warn-ink": 2,
   "--radius-10": 29,
   "--glass-pin": 2,
   "--glass-head": 2,
   "--z-page-3": 2,
   "--chip-hover": 1,
   "--space-20": 7,
   "--bar-bg": 2,
   "--radius-14": 18,
   "--z-bar": 1,
   "--z-toast": 1,
   "--toast-bg": 2,
   "--z-overlay": 2,
   "--space-24": 4,
   "--z-drawer": 1,
   "--z-page-5": 1,
   "--dup-hue": 1,
   "--err": 4,
   "--switch-off": 1,
   "--tag-on-ink": 1,
   "--badge-bg": 1,
   "--badge-ink": 1,
   "--tone-indigo-bg": 1,
   "--tone-indigo-ink": 1,
   "--tone-blue-bg": 1,
   "--tone-blue-ink": 1,
   "--tone-lilac-bg": 1,
   "--tone-lilac-ink": 1,
   "--tone-purple-bg": 1,
   "--tone-purple-ink": 1,
   "--tone-clay-bg": 1,
   "--tone-clay-ink": 1,
   "--radius-2": 6,
   "--tone-sand-bg": 1,
   "--fs-14": 3,
   "--hint-bg": 1,
   "--hint-ink": 2,
   "--hint-ring": 1,
   "--hint-bg-hover": 1,
   "--palette-hover": 1,
   "--warn": 13,
   "--z-menu": 2,
   "--tone-steel-ink": 1,
   "--tone-steel-bg": 1,
   "--tone-amber-ink": 1,
   "--tone-amber-bg": 1,
   "--mono": 4,
   "--ok": 3,
   "--bad": 4,
   "--radius-18": 5,
   "--sheet-shadow": 3,
   "--r18": 4,
   "--r18-soft": 1,
   "--r18-on": 1,
   "--select-arrow": 1,
   "--radius-12": 17,
   "--seg-on": 3,
   "--menu-group": 1,
   "--wash-hover": 1,
   "--view-sw-off": 1,
   "--fs-display-17": 1,
   "--btn-hover-filter": 1,
   "--z-guide-tour": 1,
   "--tour-dim": 2,
   "--z-guide-nudge": 1,
   "--z-guide-checklist": 1,
   "--fs-display-21": 2,
   "--radius-16": 3,
   "--z-page-6": 1,
   "--fs-display-40": 1,
   "--fs-display-26": 2,
   "--fs-display-72": 1,
   "--p": 1,
   "--fs-display-28": 1,
   "--z-page-1": 1,
   "--h-28": 1,
   "--fs-display-30": 1,
   "--atag-open-bg": 1,
   "--atag-open-ink": 1,
   "--atag-open-ring": 1,
   "--atag-yes-bg": 1,
   "--atag-yes-ink": 1,
   "--atag-no-bg": 1,
   "--atag-no-ink": 1,
   "--z-splash": 1,
   "--splash-bg": 1,
   "--fs-display-32": 1,
   "--fs-display-18": 1,
   "--panel": 1,
   "--depth": 1
  },
  "groups": [
   {
    "title": "基础",
    "names": [
     "--canvas",
     "--sheet",
     "--ink",
     "--ink2",
     "--muted",
     "--faint",
     "--line",
     "--hair",
     "--soft",
     "--accent",
     "--accent-btn",
     "--accent-soft",
     "--accent-ink",
     "--warn",
     "--r18",
     "--r18-soft",
     "--sh",
     "--mono",
     "--ease",
     "--sheet-shadow"
    ]
   },
   {
    "title": "警告/提示色调：底色 + 文字，成对使用（错误条、核对提示、关系类型徽标）",
    "names": [
     "--tone-warn-bg",
     "--tone-warn-ink",
     "--tone-sand-bg",
     "--tone-sand-ink",
     "--tone-amber-bg",
     "--tone-amber-ink",
     "--tone-steel-bg",
     "--tone-steel-ink",
     "--tone-indigo-bg",
     "--tone-indigo-ink",
     "--tone-blue-bg",
     "--tone-blue-ink",
     "--tone-lilac-bg",
     "--tone-lilac-ink",
     "--tone-purple-bg",
     "--tone-purple-ink",
     "--tone-clay-bg",
     "--tone-clay-ink"
    ]
   },
   {
    "title": "图上标签芯片：自动标签（待确认/已确认/已否定）与建议标签",
    "names": [
     "--atag-open-bg",
     "--atag-open-ink",
     "--atag-open-ring",
     "--atag-yes-bg",
     "--atag-yes-ink",
     "--atag-no-bg",
     "--atag-no-ink",
     "--hint-bg",
     "--hint-ink",
     "--hint-ring",
     "--hint-bg-hover",
     "--glass-chip",
     "--glass-pin"
    ]
   },
   {
    "title": "浮层表面：磨砂条、提示条、菜单分组、启动页",
    "names": [
     "--glass-head",
     "--bar-bg",
     "--toast-bg",
     "--menu-group",
     "--splash-bg"
    ]
   },
   {
    "title": "控件状态：开关、分段控件、悬停、徽标、滚动条",
    "names": [
     "--switch-off",
     "--view-sw-off",
     "--seg-on",
     "--wash-hover",
     "--chip-hover",
     "--palette-hover",
     "--btn-hover-filter",
     "--badge-bg",
     "--badge-ink",
     "--tag-on-ink",
     "--scroll-thumb",
     "--select-arrow",
     "--r18-on"
    ]
   },
   {
    "title": "不随主题变化的状态色",
    "names": [
     "--ok",
     "--bad",
     "--err"
    ]
   }
  ]
 },
 "tokens": {
  "fsDisplay": [
   {
    "name": "--fs-display-17",
    "value": "17px",
    "uses": 1
   },
   {
    "name": "--fs-display-18",
    "value": "18px",
    "uses": 1
   },
   {
    "name": "--fs-display-21",
    "value": "21px",
    "uses": 2
   },
   {
    "name": "--fs-display-26",
    "value": "26px",
    "uses": 2
   },
   {
    "name": "--fs-display-28",
    "value": "28px",
    "uses": 1
   },
   {
    "name": "--fs-display-30",
    "value": "30px",
    "uses": 1
   },
   {
    "name": "--fs-display-32",
    "value": "32px",
    "uses": 1
   },
   {
    "name": "--fs-display-40",
    "value": "40px",
    "uses": 1
   },
   {
    "name": "--fs-display-72",
    "value": "72px",
    "uses": 1
   }
  ],
  "fs": [
   {
    "name": "--fs-10",
    "value": "10px",
    "uses": 14
   },
   {
    "name": "--fs-11",
    "value": "11px",
    "uses": 109
   },
   {
    "name": "--fs-12",
    "value": "12px",
    "uses": 136
   },
   {
    "name": "--fs-13",
    "value": "13px",
    "uses": 30
   },
   {
    "name": "--fs-14",
    "value": "14px",
    "uses": 3
   },
   {
    "name": "--fs-15",
    "value": "15px",
    "uses": 10
   },
   {
    "name": "--fs-16",
    "value": "16px",
    "uses": 2
   },
   {
    "name": "--fs-20",
    "value": "20px",
    "uses": 4
   },
   {
    "name": "--fs-22",
    "value": "22px",
    "uses": 3
   }
  ],
  "radius": [
   {
    "name": "--radius-2",
    "value": "2px",
    "uses": 6
   },
   {
    "name": "--radius-4",
    "value": "4px",
    "uses": 20
   },
   {
    "name": "--radius-6",
    "value": "6px",
    "uses": 34
   },
   {
    "name": "--radius-8",
    "value": "8px",
    "uses": 17
   },
   {
    "name": "--radius-10",
    "value": "10px",
    "uses": 29
   },
   {
    "name": "--radius-12",
    "value": "12px",
    "uses": 17
   },
   {
    "name": "--radius-14",
    "value": "14px",
    "uses": 18
   },
   {
    "name": "--radius-16",
    "value": "16px",
    "uses": 3
   },
   {
    "name": "--radius-18",
    "value": "18px",
    "uses": 5
   },
   {
    "name": "--radius-pill",
    "value": "999px",
    "uses": 48
   },
   {
    "name": "--radius-round",
    "value": "50%",
    "uses": 16
   }
  ],
  "space": [
   {
    "name": "--space-1",
    "value": "1px",
    "uses": 26
   },
   {
    "name": "--space-2",
    "value": "2px",
    "uses": 49
   },
   {
    "name": "--space-4",
    "value": "4px",
    "uses": 95
   },
   {
    "name": "--space-6",
    "value": "6px",
    "uses": 116
   },
   {
    "name": "--space-8",
    "value": "8px",
    "uses": 133
   },
   {
    "name": "--space-10",
    "value": "10px",
    "uses": 97
   },
   {
    "name": "--space-12",
    "value": "12px",
    "uses": 74
   },
   {
    "name": "--space-14",
    "value": "14px",
    "uses": 44
   },
   {
    "name": "--space-16",
    "value": "16px",
    "uses": 27
   },
   {
    "name": "--space-20",
    "value": "20px",
    "uses": 7
   },
   {
    "name": "--space-24",
    "value": "24px",
    "uses": 4
   }
  ],
  "h": [
   {
    "name": "--h-28",
    "value": "28px",
    "uses": 1
   },
   {
    "name": "--h-32",
    "value": "32px",
    "uses": 12
   },
   {
    "name": "--h-36",
    "value": "36px",
    "uses": 16
   }
  ],
  "z": [
   {
    "name": "--z-page-1",
    "value": "1",
    "uses": 1
   },
   {
    "name": "--z-page-2",
    "value": "2",
    "uses": 6
   },
   {
    "name": "--z-page-3",
    "value": "3",
    "uses": 2
   },
   {
    "name": "--z-page-4",
    "value": "4",
    "uses": 2
   },
   {
    "name": "--z-page-5",
    "value": "5",
    "uses": 1
   },
   {
    "name": "--z-page-6",
    "value": "6",
    "uses": 1
   },
   {
    "name": "--z-bar",
    "value": "10",
    "uses": 1
   },
   {
    "name": "--z-popover",
    "value": "20",
    "uses": 1
   },
   {
    "name": "--z-drawer",
    "value": "25",
    "uses": 1
   },
   {
    "name": "--z-menu",
    "value": "30",
    "uses": 2
   },
   {
    "name": "--z-overlay",
    "value": "40",
    "uses": 2
   },
   {
    "name": "--z-toast",
    "value": "60",
    "uses": 1
   },
   {
    "name": "--z-guide-checklist",
    "value": "790",
    "uses": 1
   },
   {
    "name": "--z-guide-nudge",
    "value": "800",
    "uses": 1
   },
   {
    "name": "--z-guide-tour",
    "value": "900",
    "uses": 1
   },
   {
    "name": "--z-splash",
    "value": "1000",
    "uses": 1
   }
  ]
 },
 "darkOverrideRules": 1,
 "literalHexColors": {
  "kinds": 71,
  "uses": 130,
  "table": [
   {
    "value": "#fff",
    "uses": 36,
    "examples": [
     ".primary",
     ".chip-filter.on",
     ".check",
     ".card.selected .check"
    ]
   },
   {
    "value": "#000",
    "uses": 4,
    "examples": [
     ".cast-tabrow",
     ".cast-recent"
    ]
   },
   {
    "value": "#0003",
    "uses": 3,
    "examples": [
     ".check",
     ".toast",
     ".switch::after"
    ]
   },
   {
    "value": "#76768f",
    "uses": 3,
    "examples": [
     ".work-head:focus-visible",
     ".switch:focus-visible",
     ".form-grid input:focus"
    ]
   },
   {
    "value": "#7d7d95",
    "uses": 3,
    "examples": [
     ".card.focused",
     ".marquee",
     ".card.fit.focused .fit-box"
    ]
   },
   {
    "value": "#ebebef",
    "uses": 3,
    "examples": [
     ".viewer-bar .identity-name",
     ".ask-bar.on-dark .ask-q",
     ".ask-bar.on-dark .ask-b"
    ]
   },
   {
    "value": "#0001",
    "uses": 2,
    "examples": [
     ".sidebar",
     ".inspector"
    ]
   },
   {
    "value": "#3c8d5a",
    "uses": 2,
    "examples": [
     ".aa-card.yes",
     ".aa-verdict.yes"
    ]
   },
   {
    "value": "#8a3b2c",
    "uses": 2,
    "examples": [
     ".primary.armed"
    ]
   },
   {
    "value": "#9696a1",
    "uses": 2,
    "examples": [
     ".viewer-missing"
    ]
   },
   {
    "value": "#a2a2b0",
    "uses": 2,
    "examples": [
     ".viewer-bar .alt kbd",
     ".ask-bar.on-dark .ask-q small"
    ]
   },
   {
    "value": "#b0643d",
    "uses": 2,
    "examples": [
     ".watch-chip em",
     "button.armed"
    ]
   },
   {
    "value": "#c2463d",
    "uses": 2,
    "examples": [
     ".chips .atag button[data-tagact=no]:hover",
     ".gsec-img.off i"
    ]
   },
   {
    "value": "#e0e0e6",
    "uses": 2,
    "examples": [
     ".nav.active",
     ".entity-link.active"
    ]
   },
   {
    "value": "#ffffff0d",
    "uses": 2,
    "examples": [
     ".viewer-bar .identity",
     ".viewer-bar .alt"
    ]
   },
   {
    "value": "#ffffff14",
    "uses": 2,
    "examples": [
     ".viewer-top .icon:hover",
     ".ask-bar.on-dark"
    ]
   },
   {
    "value": "#ffffff1a",
    "uses": 2,
    "examples": [
     ".viewer-bar .icon:hover",
     ".ask-bar.on-dark .ask-b"
    ]
   },
   {
    "value": "#ffffff24",
    "uses": 2,
    "examples": [
     ".viewer-bar .identity",
     ".viewer-bar .alt"
    ]
   },
   {
    "value": "#ffffff33",
    "uses": 2,
    "examples": [
     ".toast-undo:hover",
     ".viewer-bar .alt kbd"
    ]
   },
   {
    "value": "#0000000a",
    "uses": 1,
    "examples": [
     "a.sidebar-bottom:hover"
    ]
   },
   {
    "value": "#0000002e",
    "uses": 1,
    "examples": [
     ".check"
    ]
   },
   {
    "value": "#00000055",
    "uses": 1,
    "examples": [
     ".check:hover"
    ]
   },
   {
    "value": "#0000008a",
    "uses": 1,
    "examples": [
     ".dup-badge.folded"
    ]
   },
   {
    "value": "#0002",
    "uses": 1,
    "examples": [
     ".chips span"
    ]
   },
   {
    "value": "#0004",
    "uses": 1,
    "examples": [
     ".machine-dot"
    ]
   },
   {
    "value": "#0007",
    "uses": 1,
    "examples": [
     ".gsec-img i"
    ]
   },
   {
    "value": "#000a",
    "uses": 1,
    "examples": [
     ".rel-no"
    ]
   },
   {
    "value": "#121215",
    "uses": 1,
    "examples": [
     ".viewer"
    ]
   },
   {
    "value": "#13142e1c",
    "uses": 1,
    "examples": [
     ".selection-bar"
    ]
   },
   {
    "value": "#1c1c221c",
    "uses": 1,
    "examples": [
     ".combo-menu"
    ]
   },
   {
    "value": "#1c1c2324",
    "uses": 1,
    "examples": [
     "dialog"
    ]
   },
   {
    "value": "#1d1d2330",
    "uses": 1,
    "examples": [
     "dialog::backdrop"
    ]
   },
   {
    "value": "#2f8f5b",
    "uses": 1,
    "examples": [
     ".chips .atag button[data-tagact=yes]:hover"
    ]
   },
   {
    "value": "#3d3d47",
    "uses": 1,
    "examples": [
     ".primary:hover"
    ]
   },
   {
    "value": "#3d7be0",
    "uses": 1,
    "examples": [
     ".rel-img i.match"
    ]
   },
   {
    "value": "#474757",
    "uses": 1,
    "examples": [
     ".sel-count"
    ]
   },
   {
    "value": "#54545d",
    "uses": 1,
    "examples": [
     ".primary kbd"
    ]
   },
   {
    "value": "#6b6fd6",
    "uses": 1,
    "examples": [
     ".prov-bar>span"
    ]
   },
   {
    "value": "#70708a",
    "uses": 1,
    "examples": [
     ".local-dot"
    ]
   },
   {
    "value": "#76768f55",
    "uses": 1,
    "examples": [
     ".form-grid input:focus"
    ]
   },
   {
    "value": "#7a7a84",
    "uses": 1,
    "examples": [
     ".viewer-size"
    ]
   },
   {
    "value": "#7d7d9522",
    "uses": 1,
    "examples": [
     ".marquee"
    ]
   },
   {
    "value": "#7e7e96",
    "uses": 1,
    "examples": [
     ":focus-visible"
    ]
   },
   {
    "value": "#7fe0b0",
    "uses": 1,
    "examples": [
     ".cast-poster .pill i"
    ]
   },
   {
    "value": "#86869a",
    "uses": 1,
    "examples": [
     ".dot.machine"
    ]
   },
   {
    "value": "#8f8f9b",
    "uses": 1,
    "examples": [
     ".alts h3,.viewer-bar h3"
    ]
   },
   {
    "value": "#9696a2",
    "uses": 1,
    "examples": [
     ".viewer-bar .none-yet"
    ]
   },
   {
    "value": "#9696a933",
    "uses": 1,
    "examples": [
     ".modal .field input:focus"
    ]
   },
   {
    "value": "#9a6a2c",
    "uses": 1,
    "examples": [
     ".group-near"
    ]
   },
   {
    "value": "#a08563",
    "uses": 1,
    "examples": [
     ".bar-note"
    ]
   },
   {
    "value": "#a8a8b2",
    "uses": 1,
    "examples": [
     ".viewer-top"
    ]
   },
   {
    "value": "#b0573f",
    "uses": 1,
    "examples": [
     ".nsfw-dot.only"
    ]
   },
   {
    "value": "#b08a2d",
    "uses": 1,
    "examples": [
     ".pin.on"
    ]
   },
   {
    "value": "#b3b3c0",
    "uses": 1,
    "examples": [
     ".viewer-bar .add-identity"
    ]
   },
   {
    "value": "#c2413a",
    "uses": 1,
    "examples": [
     ".primary.armed"
    ]
   },
   {
    "value": "#c28a2c",
    "uses": 1,
    "examples": [
     ".local-dot.busy"
    ]
   },
   {
    "value": "#c3c3cc",
    "uses": 1,
    "examples": [
     ".nsfw-dot"
    ]
   },
   {
    "value": "#c3c3cd",
    "uses": 1,
    "examples": [
     ".viewer-bar .icon"
    ]
   },
   {
    "value": "#c3c3ce",
    "uses": 1,
    "examples": [
     ".primary kbd"
    ]
   },
   {
    "value": "#d0d0d9",
    "uses": 1,
    "examples": [
     ".viewer-top .icon"
    ]
   }
  ]
 },
 "rgbaCalls": 91,
 "fontSize": {
  "kinds": 22,
  "uses": 326,
  "table": [
   {
    "value": "0",
    "uses": 2,
    "examples": [
     ".nav-icon",
     ".section-label"
    ]
   },
   {
    "value": "9px (font 简写)",
    "uses": 1,
    "examples": [
     ".r18 text"
    ]
   },
   {
    "value": "var(--fs-10)",
    "uses": 14,
    "examples": [
     "kbd",
     ".entity-link small",
     ".sidebar-bottom",
     ".chips span"
    ]
   },
   {
    "value": "var(--fs-11)",
    "uses": 108,
    "examples": [
     ".nav-count",
     ".section-label",
     ".entity-link .entity-name",
     ".side-hint"
    ]
   },
   {
    "value": "var(--fs-12)",
    "uses": 136,
    "examples": [
     ".entity-link",
     ".breadcrumb",
     ".quiet",
     ".primary"
    ]
   },
   {
    "value": "var(--fs-13)",
    "uses": 30,
    "examples": [
     ":root",
     ".brand",
     ".icon.small",
     ".char-group h2"
    ]
   },
   {
    "value": "var(--fs-14)",
    "uses": 3,
    "examples": [
     ".pair-name a",
     ".tour-title",
     ".settings-card h2"
    ]
   },
   {
    "value": "var(--fs-15)",
    "uses": 10,
    "examples": [
     ".nav-icon",
     "#pickerSearch",
     ".dialog-top h2",
     ".about-name"
    ]
   },
   {
    "value": "var(--fs-16)",
    "uses": 2,
    "examples": [
     ".icon",
     ".rule b"
    ]
   },
   {
    "value": "var(--fs-display-17)",
    "uses": 1,
    "examples": [
     ".cc-name.muted"
    ]
   },
   {
    "value": "var(--fs-display-18)",
    "uses": 1,
    "examples": [
     ".prov-stat b"
    ]
   },
   {
    "value": "var(--fs-20)",
    "uses": 4,
    "examples": [
     "h1.tag-title",
     "h1"
    ]
   },
   {
    "value": "var(--fs-display-21)",
    "uses": 2,
    "examples": [
     ".home-dh b",
     ".home-cap b"
    ]
   },
   {
    "value": "var(--fs-22)",
    "uses": 3,
    "examples": [
     "h1",
     ".cc-name",
     ".strip-more b"
    ]
   },
   {
    "value": "var(--fs-display-26)",
    "uses": 2,
    "examples": [
     ".cast-board .cast-poster:first-child .cap b",
     ".gsec-img i"
    ]
   },
   {
    "value": "var(--fs-display-28)",
    "uses": 1,
    "examples": [
     ".cast-head h2"
    ]
   },
   {
    "value": "var(--fs-display-30)",
    "uses": 1,
    "examples": [
     ".ask-img i"
    ]
   },
   {
    "value": "var(--fs-display-32)",
    "uses": 1,
    "examples": [
     "#splash .sp-name"
    ]
   },
   {
    "value": "var(--fs-display-40)",
    "uses": 1,
    "examples": [
     ".cast-poster .rank"
    ]
   },
   {
    "value": "var(--fs-display-72)",
    "uses": 1,
    "examples": [
     ".cast-board .cast-poster:first-child .rank"
    ]
   },
   {
    "value": "inherit",
    "uses": 1,
    "examples": [
     ".net-table .link-button.strong"
    ]
   },
   {
    "value": "max(var(--fs-11),.62em)",
    "uses": 1,
    "examples": [
     ".char-work"
    ]
   }
  ]
 },
 "radius": {
  "kinds": 14,
  "uses": 212,
  "table": [
   {
    "value": "0",
    "uses": 1,
    "examples": [
     ".work-head"
    ]
   },
   {
    "value": "0 var(--radius-12) var(--radius-12) 0",
    "uses": 1,
    "examples": [
     ".cast-table tbody td:last-child"
    ]
   },
   {
    "value": "var(--radius-2)",
    "uses": 6,
    "examples": [
     ".rel-img i",
     ".cast-poster .ticks i",
     ".cast-poster .mini",
     ".cast-poster .mini i"
    ]
   },
   {
    "value": "var(--radius-4)",
    "uses": 20,
    "examples": [
     "kbd",
     ".chips span",
     ".marquee",
     ".variant-chip button"
    ]
   },
   {
    "value": "var(--radius-6)",
    "uses": 34,
    "examples": [
     ".nav",
     ".entity-link",
     ".icon",
     ".quiet"
    ]
   },
   {
    "value": "var(--radius-8)",
    "uses": 17,
    "examples": [
     ".search-wrap",
     ".card",
     ".char-cover",
     ".preview"
    ]
   },
   {
    "value": "var(--radius-10)",
    "uses": 29,
    "examples": [
     ".char-card",
     ".identity",
     ".toast",
     ".dup-badge"
    ]
   },
   {
    "value": "var(--radius-12)",
    "uses": 13,
    "examples": [
     ".primary",
     "#errorBanner,#damageBanner",
     ".identity",
     ".selection-bar .quiet,.selection-bar .icon"
    ]
   },
   {
    "value": "var(--radius-12) 0 0 var(--radius-12)",
    "uses": 1,
    "examples": [
     ".cast-table tbody td:first-child"
    ]
   },
   {
    "value": "var(--radius-14)",
    "uses": 18,
    "examples": [
     ".selection-bar",
     "dialog",
     ".nav",
     ".sidebar-bottom,a.sidebar-bottom"
    ]
   },
   {
    "value": "var(--radius-16)",
    "uses": 3,
    "examples": [
     ".char-cover",
     ".cast-poster",
     ".ask-card"
    ]
   },
   {
    "value": "var(--radius-18)",
    "uses": 5,
    "examples": [
     ".content",
     ".inspector",
     ".menu",
     ".selection-bar"
    ]
   },
   {
    "value": "var(--radius-pill)",
    "uses": 48,
    "examples": [
     ".heading-main h1 .icon.small",
     ".chip-filter",
     ".skin-chip",
     ".alt"
    ]
   },
   {
    "value": "var(--radius-round)",
    "uses": 16,
    "examples": [
     ".local-dot",
     ".nsfw-dot",
     ".check",
     ".machine-dot"
    ]
   }
  ]
 },
 "gap": {
  "kinds": 21,
  "uses": 220,
  "table": [
   {
    "value": "0",
    "uses": 1,
    "examples": [
     ".icon-seg"
    ]
   },
   {
    "value": "var(--space-1)",
    "uses": 5,
    "examples": [
     ".settings-nav",
     ".switch-line>span",
     ".view-opt span,.view-row span",
     ".cast-poster .cap"
    ]
   },
   {
    "value": "var(--space-2)",
    "uses": 18,
    "examples": [
     ".inspector-head",
     ".variant-chip",
     ".selection-bar",
     ".rule"
    ]
   },
   {
    "value": "var(--space-2) var(--space-10)",
    "uses": 1,
    "examples": [
     ".cast-wcols"
    ]
   },
   {
    "value": "var(--space-4)",
    "uses": 29,
    "examples": [
     ".heading-actions",
     ".chips",
     ".caption",
     ".skin-row"
    ]
   },
   {
    "value": "var(--space-4) var(--space-12)",
    "uses": 1,
    "examples": [
     ".cast-tagged"
    ]
   },
   {
    "value": "var(--space-6)",
    "uses": 50,
    "examples": [
     ".sidebar-bottom",
     ".nsfw-toggle",
     ".chips-row",
     ".identities"
    ]
   },
   {
    "value": "var(--space-6) var(--space-14)",
    "uses": 1,
    "examples": [
     ".fold-row"
    ]
   },
   {
    "value": "var(--space-8)",
    "uses": 44,
    "examples": [
     ".brand",
     ".entity-link",
     ".topbar",
     ".toolbar"
    ]
   },
   {
    "value": "var(--space-8) var(--space-14)",
    "uses": 1,
    "examples": [
     ".form-grid"
    ]
   },
   {
    "value": "var(--space-10)",
    "uses": 32,
    "examples": [
     ".nav",
     ".footer",
     ".char-group h2",
     ".viewer-top"
    ]
   },
   {
    "value": "var(--space-12)",
    "uses": 12,
    "examples": [
     ".toast",
     ".switch-line",
     ".setting-line",
     ".group-grid"
    ]
   },
   {
    "value": "var(--space-14)",
    "uses": 10,
    "examples": [
     ".char-grid",
     ".settings-main",
     ".rel-figs",
     ".about-head"
    ]
   },
   {
    "value": "var(--space-16)",
    "uses": 7,
    "examples": [
     ".page-heading",
     ".pair-body",
     ".modal form",
     ".cc-stats"
    ]
   },
   {
    "value": "18px",
    "uses": 1,
    "examples": [
     ".cast-head"
    ]
   },
   {
    "value": "18px var(--space-10)",
    "uses": 1,
    "examples": [
     ".cast-roster"
    ]
   },
   {
    "value": "18px var(--space-14)",
    "uses": 1,
    "examples": [
     ".char-grid"
    ]
   },
   {
    "value": "18px var(--space-16)",
    "uses": 1,
    "examples": [
     ".group-grid"
    ]
   },
   {
    "value": "var(--space-24)",
    "uses": 2,
    "examples": [
     ".viewer-bar",
     ".settings-page"
    ]
   },
   {
    "value": "26px 18px",
    "uses": 1,
    "examples": [
     ".cast-works"
    ]
   },
   {
    "value": "28px",
    "uses": 1,
    "examples": [
     ".settings-page"
    ]
   }
  ]
 },
 "padding": {
  "kinds": 126,
  "uses": 257,
  "table": [
   {
    "value": "0",
    "uses": 19,
    "examples": [
     ".preview",
     ".alt kbd",
     "dialog.palette",
     ".link-button.strong"
    ]
   },
   {
    "value": "var(--space-1) var(--space-6)",
    "uses": 12,
    "examples": [
     ".chips span",
     ".tag-on",
     ".group-traits span",
     ".unchecked-link.on"
    ]
   },
   {
    "value": "var(--space-8) var(--space-12)",
    "uses": 8,
    "examples": [
     ".primary",
     ".rule",
     ".modal .dialog-lead",
     ".quiet"
    ]
   },
   {
    "value": "var(--space-1) var(--space-8)",
    "uses": 7,
    "examples": [
     ".skin-chip",
     ".dup-badge",
     ".chips span",
     ".group-traits span"
    ]
   },
   {
    "value": "var(--space-4) var(--space-6)",
    "uses": 7,
    "examples": [
     ".heading-main>.menu-drop p",
     ".work-head",
     ".tag",
     ".num-field input"
    ]
   },
   {
    "value": "var(--space-4) var(--space-8)",
    "uses": 7,
    "examples": [
     "#watchAdd",
     ".icon-seg button",
     ".tag",
     ".home-more"
    ]
   },
   {
    "value": "var(--space-10) var(--space-12)",
    "uses": 6,
    "examples": [
     ".lib-created",
     ".ask-bar",
     ".lib-form",
     ".prov-confirm"
    ]
   },
   {
    "value": "var(--space-6)",
    "uses": 6,
    "examples": [
     ".char-card",
     ".picker-results",
     ".selection-bar",
     ".dup-copy"
    ]
   },
   {
    "value": "var(--space-8) var(--space-10)",
    "uses": 6,
    "examples": [
     ".entity-link",
     ".quiet",
     ".selection-bar",
     ".identity"
    ]
   },
   {
    "value": "0 var(--space-10)",
    "uses": 5,
    "examples": [
     ".nav",
     ".search-wrap",
     ".topbar",
     ".modal .field input"
    ]
   },
   {
    "value": "0 var(--space-4)",
    "uses": 5,
    "examples": [
     "kbd",
     ".variant-chip button",
     ".sel-count",
     ".watch-chip button"
    ]
   },
   {
    "value": "0 var(--space-8)",
    "uses": 5,
    "examples": [
     "select",
     ".menu-drop input",
     ".caption",
     ".nsfw-toggle"
    ]
   },
   {
    "value": "var(--space-10) 0",
    "uses": 5,
    "examples": [
     ".oc-split-group",
     ".settings-card details.more",
     ".settings-card .form-actions,.settings-card .about-actions",
     ".set-actions"
    ]
   },
   {
    "value": "var(--space-4) var(--space-12)",
    "uses": 5,
    "examples": [
     ".segmented button",
     ".aa-b",
     ".segmented a",
     ".settings-card button.quiet"
    ]
   },
   {
    "value": "var(--space-6) var(--space-10)",
    "uses": 5,
    "examples": [
     ".chip-filter",
     ".toast-undo",
     ".settings-nav button",
     ".picker-staged"
    ]
   },
   {
    "value": "0 var(--space-12)",
    "uses": 4,
    "examples": [
     ".content",
     ".jump",
     ".menu-drop input",
     ".lib-input"
    ]
   },
   {
    "value": "var(--space-4) var(--space-10)",
    "uses": 4,
    "examples": [
     ".staged-chip",
     ".cast-tags span",
     ".gsec-head .primary,.gsec-head .secondary,.gsec-head .quiet",
     ".quiet.small"
    ]
   },
   {
    "value": "var(--space-6) var(--space-12)",
    "uses": 4,
    "examples": [
     ".chip-filter",
     ".view-opt,.view-row",
     ".home-retry",
     ".cast-buckets button"
    ]
   },
   {
    "value": "var(--space-8) 0",
    "uses": 4,
    "examples": [
     ".task-row",
     ".switch-line",
     ".settings-card .setting-line,.settings-card .switch-line",
     ".settings-card .setting-line,.settings-card .switch-line,.settings-card a.set-link"
    ]
   },
   {
    "value": "0 var(--gutter)",
    "uses": 3,
    "examples": [
     ".content"
    ]
   },
   {
    "value": "0 var(--space-14)",
    "uses": 3,
    "examples": [
     ".cast-tab.plain",
     ".ask-foot .primary,.ask-foot .secondary",
     ".shelf-bar .watch-add input"
    ]
   },
   {
    "value": "0 var(--space-6)",
    "uses": 3,
    "examples": [
     "#pickerSearch",
     "kbd",
     ".lib-picked .quiet"
    ]
   },
   {
    "value": "var(--space-2) var(--space-4)",
    "uses": 3,
    "examples": [
     ".link-button",
     "#watchMin",
     ".chips .atag button"
    ]
   },
   {
    "value": "var(--space-6) var(--space-8)",
    "uses": 3,
    "examples": [
     ".palette-item",
     ".form-grid input",
     ".cast-wrow"
    ]
   }
  ]
 },
 "height": {
  "kinds": 48,
  "uses": 93,
  "table": [
   {
    "value": "height: 1px",
    "uses": 1,
    "examples": [
     ".section-label"
    ]
   },
   {
    "value": "min-height: 1px",
    "uses": 1,
    "examples": [
     ".grid"
    ]
   },
   {
    "value": "height: 3px",
    "uses": 2,
    "examples": [
     ".cast-poster .ticks i",
     "#splash .sp-track"
    ]
   },
   {
    "value": "height: 4px",
    "uses": 1,
    "examples": [
     ".progress"
    ]
   },
   {
    "value": "height: 5px",
    "uses": 1,
    "examples": [
     ".local-dot"
    ]
   },
   {
    "value": "height: 6px",
    "uses": 4,
    "examples": [
     ".tour-dots i",
     ".cast-poster .pill i",
     ".cast-bar i",
     ".prov-bar"
    ]
   },
   {
    "value": "height: 7px",
    "uses": 1,
    "examples": [
     ".set-status .local-dot"
    ]
   },
   {
    "value": "height: 8px",
    "uses": 6,
    "examples": [
     ".nsfw-dot",
     ".machine-dot",
     ".dot",
     ".sidebar-bottom .local-dot"
    ]
   },
   {
    "value": "height: 10px",
    "uses": 1,
    "examples": [
     ".card.selected .check::after"
    ]
   },
   {
    "value": "height: 14px",
    "uses": 2,
    "examples": [
     ".switch::after",
     ".cast-allw svg"
    ]
   },
   {
    "value": "min-height: 14px",
    "uses": 1,
    "examples": [
     ".modal .field small"
    ]
   },
   {
    "value": "height: 15px",
    "uses": 1,
    "examples": [
     ".jump svg"
    ]
   },
   {
    "value": "height: 16px",
    "uses": 5,
    "examples": [
     ".alt kbd",
     ".icon-seg svg",
     ".view-sw::after",
     ".inspector-head .icon svg"
    ]
   },
   {
    "value": "height: 17px",
    "uses": 2,
    "examples": [
     ".top-actions .icon svg,#sidebarToggle svg",
     ".strip-nav svg"
    ]
   },
   {
    "value": "height: 18px",
    "uses": 2,
    "examples": [
     ".switch",
     ".cl-ring"
    ]
   },
   {
    "value": "height: 19px",
    "uses": 1,
    "examples": [
     ".nav-icon svg"
    ]
   },
   {
    "value": "height: 20px",
    "uses": 3,
    "examples": [
     ".r18",
     ".view-sw",
     ".home-cc img,.home-cc i"
    ]
   },
   {
    "value": "height: 21px",
    "uses": 1,
    "examples": [
     ".check"
    ]
   },
   {
    "value": "height: 22px",
    "uses": 2,
    "examples": [
     ".bar-divider",
     ".palette-add"
    ]
   },
   {
    "value": "height: 24px",
    "uses": 2,
    "examples": [
     ".icon.small",
     ".pin"
    ]
   },
   {
    "value": "min-height: 24px",
    "uses": 1,
    "examples": [
     ".img-broken"
    ]
   },
   {
    "value": "height: 28px",
    "uses": 1,
    "examples": [
     ".caption"
    ]
   },
   {
    "value": "height: var(--h-28)",
    "uses": 1,
    "examples": [
     ".ask-b"
    ]
   },
   {
    "value": "height: 30px",
    "uses": 1,
    "examples": [
     ".nav-icon"
    ]
   },
   {
    "value": "height: 32px",
    "uses": 1,
    "examples": [
     ".brand .logo-mark"
    ]
   },
   {
    "value": "height: var(--h-32)",
    "uses": 9,
    "examples": [
     ".icon",
     "select",
     ".menu-drop input",
     ".inspector-head .icon"
    ]
   },
   {
    "value": "min-height: var(--h-32)",
    "uses": 1,
    "examples": [
     ".shelf-todo-row"
    ]
   },
   {
    "value": "height: 34px",
    "uses": 1,
    "examples": [
     ".palette-item img,.palette-glyph"
    ]
   },
   {
    "value": "height: 36px",
    "uses": 1,
    "examples": [
     ".footer"
    ]
   },
   {
    "value": "height: var(--h-36)",
    "uses": 14,
    "examples": [
     ".nav",
     ".search-wrap",
     "#pickerSearch",
     ".modal .field input"
    ]
   },
   {
    "value": "min-height: 36px",
    "uses": 1,
    "examples": [
     ".palette-foot"
    ]
   },
   {
    "value": "min-height: 42px",
    "uses": 1,
    "examples": [
     ".view-opt,.view-row"
    ]
   },
   {
    "value": "min-height: 44px",
    "uses": 1,
    "examples": [
     ".settings-card .setting-line,.settings-card .switch-line,.settings-card a.set-link"
    ]
   },
   {
    "value": "height: 46px",
    "uses": 1,
    "examples": [
     ".cc-bars"
    ]
   },
   {
    "value": "min-height: 46px",
    "uses": 2,
    "examples": [
     ".settings-card .setting-line,.settings-card .switch-line",
     ".shelf-row"
    ]
   },
   {
    "value": "height: 52px",
    "uses": 1,
    "examples": [
     ".topbar"
    ]
   },
   {
    "value": "height: 55px",
    "uses": 1,
    "examples": [
     ".dup-copy img"
    ]
   },
   {
    "value": "min-height: 70px",
    "uses": 1,
    "examples": [
     ".topbar"
    ]
   },
   {
    "value": "height: 72px",
    "uses": 2,
    "examples": [
     ".oc-split-thumbs img",
     ".cast-rc .ring"
    ]
   },
   {
    "value": "height: 96px",
    "uses": 3,
    "examples": [
     ".ask-img",
     ".gsec-img",
     ".gsec-rest"
    ]
   },
   {
    "value": "height: 112px",
    "uses": 1,
    "examples": [
     ".cast-head .cast-fan"
    ]
   },
   {
    "value": "height: 128px",
    "uses": 1,
    "examples": [
     "#splash .sp-icon"
    ]
   },
   {
    "value": "height: 132px",
    "uses": 1,
    "examples": [
     ".ask-img"
    ]
   },
   {
    "value": "height: 190px",
    "uses": 1,
    "examples": [
     ".cast-fan"
    ]
   },
   {
    "value": "min-height: 250px",
    "uses": 1,
    "examples": [
     ".empty"
    ]
   },
   {
    "value": "height: 280px",
    "uses": 1,
    "examples": [
     ".preview"
    ]
   },
   {
    "value": "height: 300px",
    "uses": 1,
    "examples": [
     ".preview"
    ]
   },
   {
    "value": "height: 340px",
    "uses": 1,
    "examples": [
     ".preview"
    ]
   }
  ]
 },
 "zIndex": {
  "kinds": 16,
  "uses": 25,
  "table": [
   {
    "value": "var(--z-page-1)",
    "uses": 1,
    "examples": [
     ".cast-table thead th"
    ]
   },
   {
    "value": "var(--z-page-2)",
    "uses": 6,
    "examples": [
     ".check",
     ".settings-nav",
     ".char-group h2",
     ".strip-nav"
    ]
   },
   {
    "value": "var(--z-page-3)",
    "uses": 2,
    "examples": [
     ".inspector-head",
     ".home-dh"
    ]
   },
   {
    "value": "var(--z-page-4)",
    "uses": 2,
    "examples": [
     ".marquee",
     ".cast-tabs"
    ]
   },
   {
    "value": "var(--z-page-5)",
    "uses": 1,
    "examples": [
     ".inspector"
    ]
   },
   {
    "value": "var(--z-page-6)",
    "uses": 1,
    "examples": [
     ".cast-pop"
    ]
   },
   {
    "value": "var(--z-bar)",
    "uses": 1,
    "examples": [
     ".selection-bar"
    ]
   },
   {
    "value": "var(--z-drawer)",
    "uses": 1,
    "examples": [
     ".sidebar"
    ]
   },
   {
    "value": "var(--z-guide-checklist)",
    "uses": 1,
    "examples": [
     ".checklist"
    ]
   },
   {
    "value": "var(--z-guide-nudge)",
    "uses": 1,
    "examples": [
     ".nudge"
    ]
   },
   {
    "value": "var(--z-guide-tour)",
    "uses": 1,
    "examples": [
     ".tour"
    ]
   },
   {
    "value": "var(--z-menu)",
    "uses": 2,
    "examples": [
     ".combo-menu",
     ".view-pop"
    ]
   },
   {
    "value": "var(--z-overlay)",
    "uses": 2,
    "examples": [
     ".viewer",
     ".task-pop"
    ]
   },
   {
    "value": "var(--z-popover)",
    "uses": 1,
    "examples": [
     ".menu-drop"
    ]
   },
   {
    "value": "var(--z-splash)",
    "uses": 1,
    "examples": [
     "#splash"
    ]
   },
   {
    "value": "var(--z-toast)",
    "uses": 1,
    "examples": [
     ".toast"
    ]
   }
  ]
 }
};
