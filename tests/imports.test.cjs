// 前端模块调用了别的模块导出的函数，却忘了导入（#401：设置页「图片网格」按钮调用 setFit 却没导入，点了报 ReferenceError）。
// 静态检查：在 web/static 的 app.js、core/、ui/ 里，凡是以「名字(」调用、名字是某个模块的导出、而本文件既没有导入
// 也没有自己声明它的，都报出来。按文本粗查：本地的同名变量、参数算作声明，所以只会漏报，不会因为同名而误报。
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const STATIC = path.join(__dirname, '../web/static');
const files = ['app.js', ...['core', 'ui'].flatMap(d => fs.readdirSync(path.join(STATIC, d)).filter(f => f.endsWith('.js')).map(f => `${d}/${f}`))];
const strip = s => s.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:'"`\\])\/\/.*$/gm, '$1');

test('every function a module calls from another module is imported (#401)', () => {
  const src = Object.fromEntries(files.map(f => [f, strip(fs.readFileSync(path.join(STATIC, f), 'utf8'))]));
  const exported = new Set();
  for (const s of Object.values(src)) for (const m of s.matchAll(/export\s+(?:async\s+)?(?:function\*?|const|let|class)\s+(\w+)/g)) exported.add(m[1]);
  const missing = [];
  for (const [file, s] of Object.entries(src)) {
    const local = new Set();
    for (const m of s.matchAll(/import\s*\{([^}]*)\}/g)) for (const part of m[1].split(',')) { const name = part.trim().split(/\s+as\s+/).pop(); if (name) local.add(name); }
    for (const m of s.matchAll(/import\s+(\w+)\s+from/g)) local.add(m[1]);
    for (const m of s.matchAll(/\b(?:function\*?|class)\s+(\w+)/g)) local.add(m[1]);
    for (const m of s.matchAll(/\b(?:const|let|var)\s+([^=;]+?)=/g)) for (const n of m[1].matchAll(/\w+/g)) local.add(n[0]);
    for (const m of s.matchAll(/,\s*(\w+)\s*=(?!=)/g)) local.add(m[1]);
    for (const m of s.matchAll(/\(([^()]*)\)\s*=>|\bfunction\s*\w*\s*\(([^()]*)\)|(\w+)\s*=>/g)) for (const n of (m[1] || m[2] || m[3] || '').matchAll(/\w+/g)) local.add(n[0]);
    for (const m of s.matchAll(/(?<![.\w$])(\w+)\s*\(/g)) if (exported.has(m[1]) && !local.has(m[1])) missing.push(`${file}: ${m[1]}`);
  }
  assert.deepEqual([...new Set(missing)], []);
});
