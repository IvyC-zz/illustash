const {test, before} = require('node:test');
const web = require('./web.cjs');   // core/ modules (#199)
before(web.ready);
const assert = require('node:assert/strict');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const fs = require('node:fs');
const source = fs.readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const geometry = source.slice(source.indexOf('const isSquare ='), source.indexOf('// How many images the machine thinks'));
function grid(total = 103, fit = true) {
  const state = {size:185, total, fit, dataVersion:0, items:Array.from({length:total}, (_,i) => ({width:[300,800,400,1200][i%4],height:600})), route:{page:'library'}};
  const elements = {viewport:{clientWidth:1002, scrollTop:0},grid:{getBoundingClientRect:()=>({left:10,top:20})}};
  const c = {state, MAX_HEIGHT:3e7, viewOpts:{captions:true}, browsing:()=>true, $:id=>elements[id], prefs:{set(){}}, renderGrid(){}};
  vm.createContext(Object.assign(c, zh.page)); vm.runInContext(geometry+';Object.assign(this,{rowCount,rowStart,imageRatio})',c); c.layout(); return c;
}
const rowsOf=c=>Array.from({length:c.rowCount()},(_,r)=>{const out=[];for(let i=c.rowStart(r);i<c.rowStart(r+1);i++)out.push(c.cardRect(i));return out;});
test('justified rows pack around the target height, fill the width and keep each ratio; the last row stays target-sized and left-aligned',()=>{
  const c=grid(), target=c.state.cellW*4/3, rows=rowsOf(c);
  assert.equal(c.rowStart(c.rowCount()),c.state.total);
  rows.forEach((row,r)=>{
    const h=row[0].height-28; let left=0;
    for(const [k,b] of row.entries()){
      const i=c.rowStart(r)+k, item=c.state.items[i];
      assert.equal(b.height,row[0].height); assert.ok(Math.abs(b.width/h-item.width/item.height)<1e-9);
      assert.ok(Math.abs(b.left-left)<1e-8); left=b.left+b.width+c.state.gap;
      assert.equal(c.rowOf(i),r); assert.equal(c.indexAt(b.left+b.width/2+10,b.top+b.height/2+20),i);
    }
    const end=row.at(-1).left+row.at(-1).width;
    if(r<rows.length-1){ assert.ok(Math.abs(end-1000)<1e-8); assert.ok(h>target*.6 && h<target*1.6,`row ${r} height ${h} vs ${target}`); }
    else { assert.ok(Math.abs(h-target)<1e-8); assert.ok(end<1000); }
  });
  assert.ok(rows.some(row=>row.length!==rows[0].length),'row length follows the images, not a fixed column count');
});
test('a tiny result keeps normal size instead of one giant image; extreme ratios are clamped and shown whole',()=>{
  const c=grid(1); c.state.items=[{width:600,height:800}]; c.state.dataVersion++; c.layout();
  const b=c.cardRect(0); assert.ok(Math.abs(b.height-28-c.state.cellW*4/3)<1e-8); assert.ok(b.width<300);
  c.state.total=3; c.state.items=[{width:4000,height:300},{width:100,height:2000},{width:600,height:800}]; c.state.dataVersion++; c.layout();
  const [wide,tall]=[c.cardRect(0),c.cardRect(1)];
  assert.ok(Math.abs(wide.width/(wide.height-28)-2.4)<1e-9); assert.ok(Math.abs(tall.width/(tall.height-28)-.5)<1e-9);
});
test('sparse pagination estimates unloaded images from loaded ones; row lookup stays consistent after dimensions arrive',()=>{
  const c=grid(10000); c.state.items=new Array(10000); c.state.dataVersion++; c.layout();
  const old=c.rowTop(200); assert.ok(old>0 && Number.isFinite(old));
  for(let i=0;i<100;i++) c.state.items[i]={width:1600,height:900};
  c.state.dataVersion++; c.layout();
  assert.notEqual(c.rowTop(200),old); assert.ok(Math.abs(c.imageRatio(undefined)-16/9)<1e-9);
  for(const row of [0,1,199,c.rowCount()-1]) { assert.equal(c.rowAt(c.rowTop(row)+.5),row); assert.equal(c.rowOf(c.rowStart(row)),row); }
  assert.ok(c.rowTop(c.rowCount())<=3e7);
});
test('arrow keys move to the nearest image in the neighbouring justified row',()=>{
  const c=grid(), r1=c.rowStart(1), b=c.cardRect(r1);
  const up=c.verticalStep(r1,-1), u=c.cardRect(up), x=b.left+b.width/2;
  for(let i=0;i<c.rowStart(1);i++){ const o=c.cardRect(i); assert.ok(Math.abs(u.left+u.width/2-x)<=Math.abs(o.left+o.width/2-x)); }
  assert.equal(c.rowOf(c.verticalStep(r1,1)),2); assert.equal(c.verticalStep(0,-1),0);
});
test('square mode and zoom keep predictable geometry; toggling fit anchors the same image row',()=>{
  const c=grid(103,false); const cols=c.state.cols;
  assert.equal(c.cardRect(cols).top,c.state.rowH); assert.equal(c.verticalStep(0,1),cols);
  c.state.size=300;c.layout();assert.ok(c.state.cols<cols);
  const anchor=c.state.cols*3;c.$('viewport').scrollTop=c.rowTop(3);c.toggleFit();
  const row=c.rowAt(c.$('viewport').scrollTop); assert.ok(c.rowStart(row)<=anchor && anchor<c.rowStart(row+1));
  c.toggleFit();assert.equal(c.state.fit,false);
});
function homeHarness(api){
  const home={days:[{day:'2026-10-02',items:[{uuid:'old'}]}],next:'2026-10-02',gen:1,loading:false};
  const sentinel={html:'',textContent:'',insertAdjacentHTML(where,html){assert.equal(where,'beforebegin');this.html+=html;}};
  const observer={observes:0,disconnects:0,unobserve(){},observe(){this.observes++;},disconnect(){this.disconnects++;}};
  const c={home,homeObserver:observer,homeWidth:1000,state:{route:{page:'home'},nsfw:'',items:[{uuid:'old'}],total:1},
    api,URLSearchParams, $:id=>id==='homeSentinel'?sentinel:{querySelectorAll:()=>[]},homeDay:g=>`<section>${g.day}</section>`,syncStrip(){},toast(){}};
  vm.createContext(Object.assign(c, zh.page));vm.runInContext(source.slice(source.indexOf('async function moreHome()'),source.indexOf('const ratio =')),c);
  return {c,home,sentinel,observer};
}
test('home appends only new dates and items; repeated concurrent clicks share the request',async()=>{
  let resolve,calls=0; const h=homeHarness(()=>{calls++;return new Promise(r=>resolve=r);});
  const pending=h.c.moreHome(); await h.c.moreHome();assert.equal(calls,1);
  resolve({days:[{day:'2026-10-02',items:[]},{day:'2026-10-01',items:[{uuid:'new'}]},{day:'2026-10-01',items:[]}],next:'2026-10-01'});await pending;
  assert.equal(h.sentinel.html,'<section>2026-10-01</section>');assert.equal(h.home.days.length,2);assert.equal(h.c.state.items.length,2);assert.equal(h.observer.observes,1);
});
test('home errors release loading without resubscribing and permit an explicit retry',async()=>{
  let calls=0;const h=homeHarness(async()=>{calls++;if(calls===1)throw Error('failed');return {days:[],next:''};});
  await h.c.moreHome();assert.equal(h.home.loading,false);assert.equal(h.observer.observes,0);
  await h.c.moreHome();assert.equal(calls,2);assert.equal(h.sentinel.textContent,'已经到底了');
});
test('home responses after page changes are discarded',async()=>{
  let resolve;const h=homeHarness(()=>new Promise(r=>resolve=r));const pending=h.c.moreHome();
  h.c.state.route.page='library';resolve({days:[{day:'2026-10-01',items:[]}],next:''});await pending;
  assert.equal(h.home.days.length,1);assert.equal(h.sentinel.html,'');assert.equal(h.observer.observes,0);
});
test('an older home request cannot unlock or append into a new home generation',async()=>{
  let resolve;const h=homeHarness(()=>new Promise(r=>resolve=r));const pending=h.c.moreHome();
  h.home.gen++;h.home.loading=true;resolve({days:[],next:''});await pending;
  assert.equal(h.home.loading,true);assert.equal(h.home.next,'2026-10-02');
});

test('within-day requests after navigation cannot change the library or remove a newer generation lock', async()=>{
  let resolve;
  const day={day:'2026-10-02',items:[{uuid:'old'}],count:10};
  const c={home:{days:[day],gen:1},state:{route:{page:'home'},nsfw:'',items:[{uuid:'library'}],total:1},
    api:()=>new Promise(r=>resolve=r),URLSearchParams,toast(){}, $:()=>{throw Error('stale response must not query DOM');}};
  vm.createContext(Object.assign(c, zh.page));vm.runInContext(source.slice(source.indexOf('const dayLoading ='),source.indexOf('function drawHome()')),c);
  const pending=c.moreOfDay(day.day);c.state.route.page='library';resolve({items:[{uuid:'new'}]});await pending;
  assert.equal(day.items.length,1);assert.equal(c.state.items[0].uuid,'library');
  c.state.route.page='home';const old=c.moreOfDay(day.day);c.home.gen++;
  vm.runInContext("dayLoading.clear();dayLoading.add('2026-10-02')",c);
  resolve({items:[]});await old;assert.equal(vm.runInContext("dayLoading.has('2026-10-02')",c),true);
});
test('opening the app lands on 首页; the library has its own hash and old query links still open it',()=>{
  const c={location:{hash:''},URLSearchParams,prefs:{get:(k,f)=>f},LEAD_PAGES:[]};
  vm.createContext(Object.assign(c, zh.page)); vm.runInContext(source.slice(source.indexOf('function parseHash()'),source.indexOf('const LEAD_PAGES'))+';const defaultSort=()=>"newest";this.parseHash=parseHash;this.hashFor=hashFor;',c);
  for(const hash of ['','#','#/']){ c.location.hash=hash; assert.equal(c.parseHash().page,'home'); }
  c.location.hash='#/?artist=a1'; assert.deepEqual([c.parseHash().page,c.parseHash().artist],['library','a1']);
  c.location.hash=c.hashFor({page:'library',day:'2026-10-02'}); assert.equal(c.location.hash,'#/library?day=2026-10-02');
  assert.deepEqual([c.parseHash().page,c.parseHash().day],['library','2026-10-02']);
});
test('undo and a finished sync redraw the home page, whose day groups come from their own read', async () => {
  const calls = [], {edits, setLatest, undoLatest, undoneText} = web.core();
  edits.latest = {id: 7, label: '已将 1 张移出 A'};
  globalThis.fetch = async () => ({status: 200, ok: true, json: async () => ({undo: null})});
  const c = {
    state: {route: {page: 'home'}, sync: {status: 'scanning'}, focused: null, viewer: -1},
    edits, setLatest, undoLatest, undoneText, toast() {}, async reload() { calls.push('reload'); },
    async refreshMeta() {}, error() {}, readError: e => e, renderHome(keep) { calls.push(['home', keep]); },
    renderIdentity() {}, renderGroups() {}, renderArtistAudit() {}, inspect() {}, selCount: () => 0, renderTasks() {}, guideRefresh() {},
    SYNC_ACTIVE: ['scanning'], number: String, clearTimeout() {}, setTimeout() {}, panelPage: p => p === 'home',
    async api() { return {state: {status: 'done', imported: 1}, history: [], damaged: null}; },
  };
  vm.createContext(Object.assign(c, zh.page));
  vm.runInContext(source.slice(source.indexOf('async function undo()'), source.indexOf('async function sweep()')) + ';this.undo = undo;', c);
  await c.undo();
  assert.deepEqual(calls, ['reload', ['home', true]]);
  assert.equal(edits.latest, null);
  calls.length = 0;
  // The sync's poller is core/jobs.js; what its end redraws is app.js's onJob listener.
  const {state, pollSync, onJob, SYNC_ACTIVE} = web.core();
  Object.assign(state, {route: {page: 'home'}, sync: {status: 'scanning'}});
  globalThis.fetch = async () => ({status: 200, ok: true, json: async () => ({state: {status: 'done', imported: 1}, history: [], damaged: null})});
  const g = {state, SYNC_ACTIVE, onJob: fn => (g.off = onJob(fn)), async refreshMeta() {}, error() {}, readError: e => e, guideRefresh() {},
    panelPage: p => p === 'home', selCount: () => 0, reload() { calls.push('reload'); }, renderHome(keep) { calls.push(['home', keep]); }};
  vm.createContext(g);
  vm.runInContext(source.slice(source.indexOf('// What a background job'), source.indexOf('// Name text queries')), g);
  await pollSync();
  g.off();
  assert.deepEqual(calls, [['home', true]]);
});
