"""Standalone local-only blind review worksheet; exports JSONL, never writes library decisions."""
import hashlib
import json
from pathlib import Path


def write_worksheet(path, queue, identities):
    data = json.dumps({'queue': queue, 'identities': identities}, ensure_ascii=False).replace('<', '\\u003c')
    key = hashlib.sha256(data.encode()).hexdigest()[:16]
    html = '''<!doctype html><html lang="zh"><meta charset="utf-8"><title>分类验证盲审</title>
<style>body{font:16px system-ui;margin:32px;background:#fafafa;color:#222}header{position:sticky;top:0;background:#fafafa;padding:12px 0}button,input,select{font:inherit;margin:6px;padding:6px}article{display:flex;gap:24px;border-top:1px solid #ddd;padding:20px 0}img{width:260px;height:300px;object-fit:contain}small{color:#555}input[type=text]{width:440px}.note{max-width:900px}</style>
<header><h1>分类验证盲审</h1><button id="export">导出人工标注 JSONL</button><span id="progress"></span></header>
<p class="note">样本尚未标注；此页面不显示机器预测，也不改图库。填写你能确认的角色；不认识就保留“不确定”。只有确实识别了画面中的全部角色，才勾选“完整”。角色字段接受下拉列表中的内部 ID，多角色用英文逗号分隔。完成后导出；不要把未审核或不完整的角色列表当负样本。</p>
<datalist id="identities"></datalist><main id="rows"></main>
<script id="data" type="application/json">__DATA__</script>
<script>
const data=JSON.parse(document.getElementById('data').textContent),key='classification-review-__KEY__';
let rows=data.queue;try{const saved=JSON.parse(localStorage.getItem(key));if(Array.isArray(saved)&&saved.length===rows.length)rows=saved}catch(e){}
const ids=document.getElementById('identities');for(const [id,name] of data.identities){const o=document.createElement('option');o.value=id;o.label=name;ids.append(o)}
function save(){try{localStorage.setItem(key,JSON.stringify(rows))}catch(e){}document.getElementById('progress').textContent=rows.filter(r=>r.reviewed).length+' / '+rows.length+' 已审核'}
for(const r of rows){const card=document.createElement('article'),link=document.createElement('a'),img=document.createElement('img'),fields=document.createElement('div');
link.href=r.image_url;link.target='_blank';img.src=r.image_url.replace('size=full','size=thumb');img.loading='lazy';img.alt=r.uuid;link.append(img);card.append(link,fields);
const title=document.createElement('small');title.textContent=r.uuid;fields.append(title,document.createElement('br'));
const people=document.createElement('select');for(const [v,t] of [['unknown','人数不确定'],['solo','单人'],['multi','多人'],['none','没有人物']]){const o=document.createElement('option');o.value=v;o.textContent=t;people.append(o)}people.value=r.people||'unknown';people.onchange=()=>{r.people=people.value;save()};fields.append(people,document.createElement('br'));
const chars=document.createElement('input');chars.type='text';chars.setAttribute('list','identities');chars.placeholder='角色 ID（逗号分隔；不认识可留空）';chars.value=r.characters.join(',');chars.onchange=()=>{r.characters=chars.value.split(',').map(s=>s.trim()).filter(Boolean);save()};fields.append(chars,document.createElement('br'));
for(const [k,t] of [['complete','全部角色均已识别（或确定无人）'],['reviewed','已人工审核']]){const label=document.createElement('label'),box=document.createElement('input');box.type='checkbox';box.checked=!!r[k];box.onchange=()=>{r[k]=box.checked;r.people=people.value;save()};label.append(box,document.createTextNode(t));fields.append(label,document.createElement('br'))}document.getElementById('rows').append(card)}
document.getElementById('export').onclick=()=>{const blob=new Blob([rows.map(r=>JSON.stringify(r)).join('\\n')+'\\n'],{type:'application/x-ndjson'}),url=URL.createObjectURL(blob),a=document.createElement('a');a.href=url;a.download='classification-gold.jsonl';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000)};save();
</script></html>'''
    Path(path).write_text(html.replace('__DATA__', data).replace('__KEY__', key))
