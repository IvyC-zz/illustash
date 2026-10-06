'use strict';
// Launch screen. A folded strip of paper opens round the base into the Illustash box, a card drops in,
// the strip's end tucks over its start and the box eases into the icon; then the name comes up. While the
// app is still loading, the cards in the box are flipped forward one by one; once it is ready the last flip
// brings the cover card back and the icon and name fly to the sidebar's brand mark.
// app.js reports progress through window.splash.status(text) and window.splash.ready().
// The desktop shell's start page plays the same animation while the local service starts (progress, settled, halt),
// then opens the library with ?launched: that load starts from the finished icon instead of folding again.
(() => {
  const root = document.getElementById('splash');
  if (!root) return;
  const launched = new URLSearchParams(location.search).has('launched');
  const MADE = 3.2;                                         // the icon and name are up and the status shows
  const NS = 'http://www.w3.org/2000/svg';
  const mk = (tag, attrs = {}, parent) => { const e = document.createElementNS(NS, tag); for (const k in attrs) e.setAttribute(k, attrs[k]); if (parent) parent.appendChild(e); return e; };
  const f = (n) => +n.toFixed(3);
  const clamp = (x) => Math.max(0, Math.min(1, x));
  const seg = (t, a, b) => clamp((t - a) / (b - a));
  const inOut = (x) => x < .5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2;
  const outCubic = (x) => 1 - Math.pow(1 - x, 3);
  const bump = (t, a, b) => (t > a && t < b) ? Math.sin(seg(t, a, b) * Math.PI) : 0;

  // ---------- the icon's artwork (docs/assets/icon-transparent.svg) ----------
  const BOX = {
    back: 'M416 418L560 330C603 303 644 302 689 327L828 412C872 438 898 466 899 509C900 540 900 567 900 595V705C901 756 882 792 844 817L687 908C642 931 603 931 561 906L416 822C374 794 355 760 355 707V583C355 538 355 497 371 463C379 445 395 431 416 418Z',
    right: 'M899 509C900 540 900 567 900 595V705C901 756 882 792 844 817L782 855C801 802 784 759 760 731C738 704 710 677 672 656L694 666C739 687 774 677 806 649L849 603C880 570 900 542 899 509Z',
    front: 'M371 463C355 497 355 538 355 583V707C355 760 374 794 416 822L561 906C603 931 642 931 687 908L782 854C800 807 786 762 760 731C738 704 710 677 672 656L417 538C379 519 359 495 371 463Z',
    rim: 'M371 463C359 495 379 519 417 535L695 663C739 684 773 674 804 647L847 601C878 568 900 542 899 509C902 542 882 572 851 605L808 651C775 680 739 690 693 669L416 541C377 522 357 495 371 463Z'
  };
  const DEFS = `
    <linearGradient id="sp-inside" x1="517" y1="312" x2="728" y2="664" gradientUnits="userSpaceOnUse"><stop stop-color="#b3b6f3"/><stop offset=".36" stop-color="#7f83dc"/><stop offset=".7" stop-color="#4f58b3"/><stop offset="1" stop-color="#293688"/></linearGradient>
    <linearGradient id="sp-right" x1="702" y1="583" x2="907" y2="730" gradientUnits="userSpaceOnUse"><stop stop-color="#e5e3fb"/><stop offset=".43" stop-color="#bbbbf1"/><stop offset="1" stop-color="#858ae1"/></linearGradient>
    <linearGradient id="sp-front" x1="367" y1="502" x2="767" y2="920" gradientUnits="userSpaceOnUse"><stop stop-color="#e2e1fb"/><stop offset=".32" stop-color="#c0bff5"/><stop offset=".66" stop-color="#9597e9"/><stop offset="1" stop-color="#5b68d2"/></linearGradient>
    <radialGradient id="sp-shade" cx="0" cy="0" r="1" gradientTransform="translate(818 805) rotate(-155) scale(171 239)" gradientUnits="userSpaceOnUse"><stop stop-color="#3447b8" stop-opacity=".38"/><stop offset="1" stop-color="#5960c8" stop-opacity="0"/></radialGradient>
    <linearGradient id="sp-rim" x1="383" y1="511" x2="898" y2="568" gradientUnits="userSpaceOnUse"><stop stop-color="#fff" stop-opacity=".92"/><stop offset=".74" stop-color="#fbfafe"/><stop offset="1" stop-color="#e4e2fb" stop-opacity=".65"/></linearGradient>
    <linearGradient id="sp-glint" x1="0" y1="0" x2="1" y2="0"><stop stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity=".9"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>
    <linearGradient id="sp-edge" x1="0" y1="0" x2="0" y2=".7" gradientUnits="userSpaceOnUse"><stop stop-color="#fff" stop-opacity=".85"/><stop offset="1" stop-color="#fff" stop-opacity=".1"/></linearGradient>
    <filter id="sp-ground" x="-50%" y="-300%" width="200%" height="700%"><feGaussianBlur stdDeviation="16"/></filter>
    <clipPath id="sp-hex"><path d="${BOX.back}"/><rect x="-2000" y="-3000" width="5000" height="3470"/></clipPath>
    <clipPath id="sp-bands"><path d="${BOX.front}"/><path d="${BOX.right}"/></clipPath>`;

  // ---------- isometric world: the icon's hexagon is a box seen from above ----------
  const O = [354, 465], C30 = .8660254, deg = Math.PI / 180;
  const proj = (p) => [O[0] + C30 * (p[0] + p[2]), O[1] + .5 * (p[2] - p[0]) - p[1]];
  const projV = (v) => [C30 * (v[0] + v[2]), .5 * (v[2] - v[0]) - v[1]];
  const add = (a, b) => a.map((x, i) => x + b[i]), sub = (a, b) => a.map((x, i) => x - b[i]), mul = (a, s) => a.map(x => x * s);
  const dot3 = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
  const norm = (a) => mul(a, 1 / Math.hypot(...a));

  // the card standing in the box, solved so it lands exactly on the icon's triangle; theta tips it forward
  const P0 = [-19.5, 0, 64.5], U0 = [310.6, 0, 108.6], V0 = [-44.3, -251.9, 126.8], KxV = cross(norm(U0), V0);
  const pose = (theta = 0, h = 0) => {
    const V = add(mul(V0, Math.cos(theta)), mul(KxV, Math.sin(theta)));
    return {P: proj(add(sub(add(P0, V0), V), [0, h, 0])), U: projV(U0), V: projV(V)};
  };
  const R_ = .14, KR = R_ * .5523;
  const CARD = `M${R_} 0H${f(1 - R_)}C${f(1 - R_ + KR)} 0 1 ${f(R_ - KR)} 1 ${R_}V${f(1 - R_)}C1 ${f(1 - R_ + KR)} ${f(1 - R_ + KR)} 1 ${f(1 - R_)} 1H${R_}C${f(R_ - KR)} 1 0 ${f(1 - R_ + KR)} 0 ${f(1 - R_)}V${R_}C0 ${f(R_ - KR)} ${f(R_ - KR)} 0 ${R_} 0Z`;
  const matrix = (s) => `matrix(${f(s.U[0])} ${f(s.U[1])} ${f(s.V[0])} ${f(s.V[1])} ${f(s.P[0])} ${f(s.P[1])})`;
  const OFF = [0, .2375, .45, .625, 1];
  const PAL = {
    lav:  ['#fbfafe', '#ebeafc', '#cdccf7', '#9795e6', '#7c7edb'],
    rose: ['#fff8fb', '#ffe3ee', '#ffbfd6', '#f28ab0', '#e2659a'],
    mint: ['#f5fffb', '#dcf7ec', '#aeeccf', '#62d1a6', '#3bb98a'],
    sky:  ['#f6faff', '#deebff', '#b5ceff', '#7ea6f7', '#5a86ee'],
    peach:['#fffaf4', '#ffefdf', '#ffd4ad', '#f7a96f', '#ef8b4c']
  };

  // ---------- DOM ----------
  const svg = root.querySelector('.sp-icon svg'), iconBox = root.querySelector('.sp-icon');
  const nameEl = root.querySelector('.sp-name'), statusEl = root.querySelector('.sp-status');
  const textEl = root.querySelector('.sp-text'), fillEl = root.querySelector('.sp-fill'), bg = root.querySelector('.sp-bg');
  mk('defs', {}, svg).innerHTML = DEFS;
  const ground = mk('ellipse', {cx: 627, cy: 950, rx: 240, ry: 22, fill: '#3b3e9c', filter: 'url(#sp-ground)', opacity: 0}, svg);
  const back = mk('path', {d: BOX.back, fill: 'url(#sp-inside)', opacity: 0}, svg);
  const cardsG = mk('g', {'clip-path': 'url(#sp-hex)'}, svg);
  const card = (pal) => {
    const g = mk('linearGradient', {id: 'sp-card-' + pal + Math.random().toString(36).slice(2, 6), gradientUnits: 'userSpaceOnUse', x1: .57, y1: 0, x2: .13, y2: 1.06}, svg.querySelector('defs'));
    const stops = OFF.map((o, i) => mk('stop', {offset: o, 'stop-color': PAL[pal][i]}, g));
    const el = mk('g', {}, cardsG);
    mk('path', {d: CARD, fill: `url(#${g.id})`}, el);
    const shade = mk('path', {d: CARD, fill: '#26206b', opacity: 0}, el);
    mk('path', {d: CARD, stroke: 'url(#sp-edge)', 'stroke-width': 1.2, 'vector-effect': 'non-scaling-stroke'}, el);
    return {el, pal, set(s, {shadeA = 0, show = true, opacity = 1} = {}) {
      el.setAttribute('display', show ? 'inline' : 'none'); el.setAttribute('opacity', f(opacity));
      el.setAttribute('transform', matrix(s)); shade.setAttribute('opacity', f(shadeA));
    }, color(p) { if (p !== this.pal) { this.pal = p; stops.forEach((st, i) => st.setAttribute('stop-color', PAL[p][i])); } }};
  };
  const behind = card('rose'), front = card('lav');
  const bands = mk('g', {opacity: 0}, svg);
  mk('path', {d: BOX.right, fill: 'url(#sp-right)'}, bands);
  mk('path', {d: BOX.front, fill: 'url(#sp-front)'}, bands); mk('path', {d: BOX.front, fill: 'url(#sp-shade)'}, bands); mk('path', {d: BOX.rim, fill: 'url(#sp-rim)'}, bands);
  const glint = mk('polygon', {points: '230,380 380,380 260,960 110,960', fill: 'url(#sp-glint)', opacity: 0}, mk('g', {'clip-path': 'url(#sp-bands)'}, bands));
  const squashBands = (s) => bands.setAttribute('transform', s ? `translate(627 930) scale(${f(1 + .022 * s)} ${f(1 - .035 * s)}) translate(-627 -930)` : '');
  const rim = (p) => { glint.setAttribute('opacity', p > 0 && p < 1 ? f(.55 * Math.sin(p * Math.PI)) : 0); glint.setAttribute('transform', `translate(${f(p * 820)} 0)`); };
  nameEl.innerHTML = [...nameEl.textContent.trim()].map(ch => `<span>${ch}</span>`).join('');
  const letters = [...nameEl.children];

  // ---------- the strip: soft paper that wraps a rounded box ----------
  // Bends have the icon's corner radius (RAD) once the box is closed; they start as crisp creases (radius 0).
  const H = 315, RAD = 73, HALF = 178.75, CX = 157.5, CZ = 157.5;
  const XMIN = CX - HALF, XMAX = CX + HALF, ZMIN = CZ - HALF, ZMAX = CZ + HALF;
  const CORN = [[XMAX - RAD, ZMAX - RAD], [XMAX - RAD, ZMIN + RAD], [XMIN + RAD, ZMIN + RAD], [XMIN + RAD, ZMAX - RAD]];
  const nrm = (al) => [Math.sin(al), 0, Math.cos(al)];
  const rotY0 = (a) => [Math.cos(a), 0, -Math.sin(a)];
  const FOLD = {top: 60, bottom: 180};                 // the strip's end is cut on a slant: the icon's fold
  const VIEW = [-1, 1, 1], LIGHT = norm([-.6, 1, .9]);
  const arcPts = (k, a0, a1, y, steps = 10) => Array.from({length: steps + 1}, (_, j) => {
    const n = nrm((a0 + (a1 - a0) * j / steps) * deg); return [CORN[k][0] + RAD * n[0], y, CORN[k][1] + RAD * n[2]];
  });
  const LOOP = (() => {
    const pts = [{p: [XMIN + RAD, -H, ZMAX]}];
    for (let k = 0; k < 4; k++) {
      const n0 = nrm(k * 90 * deg), start = [CORN[k][0] + RAD * n0[0], -H, CORN[k][1] + RAD * n0[2]], prev = pts[pts.length - 1].p;
      for (let j = 1; j < 24; j++) pts.push({p: add(prev, mul(sub(start, prev), j / 24))});
      for (let j = 0; j <= 30; j++) { const n = nrm((k * 90 + j * 3) * deg); pts.push({p: [CORN[k][0] + RAD * n[0], -H, CORN[k][1] + RAD * n[2]]}); }
    }
    return pts;
  })();
  const clipHalf = (poly, g) => {
    const out = [];
    poly.forEach((A, i) => { const B = poly[(i + 1) % poly.length], ga = g(A), gb = g(B);
      if (ga >= 0) out.push(A);
      if ((ga >= 0) !== (gb >= 0)) { const u = ga / (ga - gb); out.push([A[0] + (B[0] - A[0]) * u, A[1] + (B[1] - A[1]) * u]); } });
    return out;
  };
  // th[i]: the i-th crease, 180 = folded flat back on the previous side, 90 = round a corner of the box
  const stripFacets = (th, rr) => {
    const x0 = XMIN + rr, SIDE = 2 * HALF - 2 * rr;
    const segs = []; let p = [x0, 0, ZMAX], hd = 0, s = 0;
    const flat = (len) => { const d = rotY0(hd), n = Math.max(1, Math.ceil(len / 9)); for (let j = 0; j < n; j++) { const q = add(p, mul(d, len / n)); segs.push({s0: s, s1: s + len / n, p0: p, p1: q, n0: nrm(hd), n1: nrm(hd)}); p = q; s += len / n; } };
    const bend = (a) => {
      const tot = a * deg, steps = Math.max(1, Math.ceil(a / 3)), r = Math.max(rr, .001), c = add(p, mul(nrm(hd), -r));
      for (let j = 0; j < steps; j++) {
        const a0 = hd + tot * j / steps, a1 = hd + tot * (j + 1) / steps;
        segs.push({s0: s, s1: s + r * (a1 - a0), p0: add(c, mul(nrm(a0), r)), p1: add(c, mul(nrm(a1), r)), n0: nrm(a0), n1: nrm(a1)}); s += r * (a1 - a0);
      }
      p = add(c, mul(nrm(hd + tot), r)); hd += tot;
    };
    flat(SIDE); bend(th[1]); flat(SIDE); bend(th[2]); flat(SIDE); bend(th[3]); flat(SIDE); bend(th[4]);
    const sEnd = s; flat(FOLD.bottom - x0 + 10);
    const e0 = sEnd + FOLD.top - x0, e1 = sEnd + FOLD.bottom - x0;
    const gEnd = ([sv, y]) => (e0 + (e1 - e0) * (-y / H)) - sv;
    const out = [];
    segs.forEach((g, idx) => {
      const poly = clipHalf([[g.s0, 0], [g.s1, 0], [g.s1, -H], [g.s0, -H]], gEnd);
      if (poly.length < 3) return;
      const w = poly.map(([sv, y]) => { const u = (sv - g.s0) / (g.s1 - g.s0 || 1); return [g.p0[0] + (g.p1[0] - g.p0[0]) * u, y, g.p0[2] + (g.p1[2] - g.p0[2]) * u]; });
      out.push({w, n: norm(add(g.n0, g.n1)), right: g.s1 <= SIDE + .01, key: idx});
    });
    return out;
  };

  // facets are flat-shaded with the icon's gradient colour at their centre, darkened as they turn from the light
  const GR = {
    front: {a: [367, 502], b: [767, 920], st: [[0, '#e2e1fb'], [.32, '#c0bff5'], [.66, '#9597e9'], [1, '#5b68d2']]},
    right: {a: [702, 583], b: [907, 730], st: [[0, '#e5e3fb'], [.43, '#bbbbf1'], [1, '#858ae1']]},
    inside: {a: [517, 312], b: [728, 664], st: [[0, '#b3b6f3'], [.36, '#7f83dc'], [.7, '#4f58b3'], [1, '#293688']]}
  };
  const hexRGB = (h) => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));
  const mixRGB = (a, b, x) => a.map((v, i) => v + (b[i] - v) * clamp(x));
  const rgb = (c) => `rgb(${c.map(v => Math.round(v)).join(',')})`;
  const gradAt = (g, p) => {
    const d = [g.b[0] - g.a[0], g.b[1] - g.a[1]], u = clamp(((p[0] - g.a[0]) * d[0] + (p[1] - g.a[1]) * d[1]) / (d[0] * d[0] + d[1] * d[1]));
    for (let i = 1; i < g.st.length; i++) if (u <= g.st[i][0]) return mixRGB(hexRGB(g.st[i - 1][1]), hexRGB(g.st[i][1]), (u - g.st[i - 1][0]) / (g.st[i][0] - g.st[i - 1][0]));
    return hexRGB(g.st[g.st.length - 1][1]);
  };
  const PALE = hexRGB('#dddcf6'), SHADOW = hexRGB('#232873'), CARD_DEPTH = -57;
  const floorG = mk('g', {}), paperBehind = mk('g', {}), paperFront = mk('g', {});
  svg.insertBefore(floorG, cardsG); svg.insertBefore(paperBehind, cardsG); svg.insertBefore(paperFront, bands);
  const floor = mk('path', {}, floorG);
  const floorPath = (rr) => {
    const pts = []; for (let k = 0; k < 4; k++) { const c = [[XMAX - rr, ZMAX - rr], [XMAX - rr, ZMIN + rr], [XMIN + rr, ZMIN + rr], [XMIN + rr, ZMAX - rr]][k]; for (let j = 0; j <= 12; j++) { const n = nrm((k * 90 + j * 7.5) * deg); pts.push(proj([c[0] + rr * n[0], -H, c[1] + rr * n[2]])); } }
    return 'M' + pts.map(p => p.map(f).join(' ')).join('L') + 'Z';
  };
  const pool = new Map();
  const drawPaper = (facets, {closed, lift}) => {
    const used = new Set();
    const items = facets.map(F => {
      const w = F.w.map(q => [q[0], q[1] + lift, q[2]]), sc = w.map(proj);
      const outer = dot3(F.n, VIEW) > 0, lit = dot3(outer ? F.n : mul(F.n, -1), LIGHT);
      if (!pool.has(F.key)) pool.set(F.key, mk('path', {}));
      const el = pool.get(F.key); used.add(F.key);
      // each facet reaches a few units past its neighbours (about a pixel at icon size), so seams never open
      const c0 = mul(sc.reduce((a, b) => [a[0] + b[0], a[1] + b[1]]), 1 / sc.length);
      const ext = (p, q) => { const v = [p[0] - q[0], p[1] - q[1]], l = Math.hypot(...v) || 1; return [p[0] + v[0] / l * 4, p[1] + v[1] / l * 4]; };
      const grown = sc.length === 4 ? [ext(sc[0], sc[1]), ext(sc[1], sc[0]), ext(sc[2], sc[3]), ext(sc[3], sc[2])] : sc.map(p => ext(p, c0));
      el.setAttribute('d', 'M' + grown.map(p => p.map(f).join(' ')).join('L') + 'Z');
      let col = outer ? gradAt(F.right ? GR.right : GR.front, c0) : mixRGB(PALE, gradAt(GR.inside, c0), closed);
      el.setAttribute('fill', rgb(mixRGB(col, SHADOW, .45 * clamp((.42 - lit) / .8))));
      return {el, depth: dot3(mul(w.reduce((a, b) => add(a, b)), 1 / w.length), VIEW)};
    });
    items.sort((a, b) => a.depth - b.depth).forEach(({el, depth}) => (depth < CARD_DEPTH ? paperBehind : paperFront).appendChild(el));
    pool.forEach((el, k) => { if (!used.has(k)) el.remove(); });
  };

  // ---------- hand-over: the rounded box's outline eases into the icon's hand-drawn one ----------
  const NP = 160;
  const samplePoly = (pts) => {
    const segs = pts.map((p, i) => [p, pts[(i + 1) % pts.length]]), lens = segs.map(([a, b]) => Math.hypot(b[0] - a[0], b[1] - a[1]));
    const total = lens.reduce((a, b) => a + b), out = [];
    for (let k = 0; k < NP; k++) { let d = total * k / NP, i = 0; while (i < lens.length - 1 && d > lens[i]) { d -= lens[i]; i++; }
      const [a, b] = segs[i], u = lens[i] ? d / lens[i] : 0; out.push([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]); }
    return out;
  };
  const area = (pts) => pts.reduce((s, p, i) => { const q = pts[(i + 1) % pts.length]; return s + p[0] * q[1] - q[0] * p[1]; }, 0);
  const sampleIcon = (d, like) => {
    const el = mk('path', {d}, svg), total = el.getTotalLength();
    let pts = Array.from({length: NP}, (_, k) => { const q = el.getPointAtLength(total * k / NP); return [q.x, q.y]; });
    el.remove();
    if (Math.sign(area(pts)) !== Math.sign(area(like))) pts.reverse();
    let best = 0, bestE = Infinity;
    for (let o = 0; o < NP; o++) { let e = 0; for (let k = 0; k < NP; k += 4) { const a = like[k], b = pts[(k + o) % NP]; e += (a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2; } if (e < bestE) { bestE = e; best = o; } }
    return pts.map((_, k) => pts[(k + best) % NP]);
  };
  const hull = (pts) => {
    const P = pts.slice().sort((a, b) => a[0] - b[0] || a[1] - b[1]), cr = (o, a, b) => (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    const lo = [], up = [];
    for (const p of P) { while (lo.length >= 2 && cr(lo[lo.length - 2], lo[lo.length - 1], p) <= 0) lo.pop(); lo.push(p); }
    for (const p of P.reverse()) { while (up.length >= 2 && cr(up[up.length - 2], up[up.length - 1], p) <= 0) up.pop(); up.push(p); }
    return lo.slice(0, -1).concat(up.slice(0, -1));
  };
  const morph = (() => {
    const ring = (y) => LOOP.map(q => proj([q.p[0], y, q.p[2]]));
    const shapes = {
      back: hull([...ring(0), ...ring(-H)]),
      front: [...arcPts(2, 225, 270, 0), ...arcPts(3, 270, 360, 0), [FOLD.top, 0, ZMAX], [FOLD.bottom, -H, ZMAX], ...arcPts(3, 360, 270, -H), ...arcPts(2, 270, 225, -H)].map(proj),
      right: [[FOLD.top, 0, ZMAX], ...arcPts(0, 0, 45, 0), ...arcPts(0, 45, 0, -H), [FOLD.bottom, -H, ZMAX]].map(proj)
    };
    const fills = {back: 'url(#sp-inside)', right: 'url(#sp-right)', front: 'url(#sp-front)'};
    const M = {};
    for (const k of ['back', 'right', 'front']) {
      const a = samplePoly(shapes[k]), b = sampleIcon(BOX[k], a), el = mk('path', {fill: fills[k]});
      svg.insertBefore(el, k === 'back' ? cardsG : bands);
      M[k] = {a, b, el};
    }
    const shade = mk('path', {fill: 'url(#sp-shade)'}), rimEl = mk('path', {d: BOX.rim, fill: 'url(#sp-rim)'});
    svg.insertBefore(shade, bands); svg.insertBefore(rimEl, bands);
    return (x, show, fade) => {
      for (const k of ['back', 'right', 'front']) {
        const {a, b, el} = M[k];
        el.style.display = show ? '' : 'none';
        if (!show) continue;
        el.setAttribute('opacity', f(fade));
        el.setAttribute('d', 'M' + a.map((p, i) => [f(p[0] + (b[i][0] - p[0]) * x), f(p[1] + (b[i][1] - p[1]) * x)].join(' ')).join('L') + 'Z');
      }
      shade.style.display = rimEl.style.display = show ? '' : 'none';
      if (show) { shade.setAttribute('d', M.front.el.getAttribute('d')); shade.setAttribute('opacity', f(x * fade)); rimEl.setAttribute('opacity', f(seg(x, .4, 1) * fade)); }
    };
  })();

  // ---------- timeline ----------
  const P = .6, LS = 3.0;                                   // one card flip while loading; loading starts once the name is up
  const CYCLE = ['rose', 'mint', 'sky', 'peach'];
  const cardAt = (k) => k === 0 ? 'lav' : CYCLE[(k - 1) % CYCLE.length];
  const flipAngle = (x) => {
    if (x <= 0) return 0; if (x >= 1) return 106 * deg;
    if (x < .2) return -4 * deg * Math.sin(x / .2 * Math.PI / 2);
    const y = (x - .2) / .8; return (-4 + 110 * y * y) * deg;
  };
  let readyAt = null, plan = null;                          // plan: {kEnd, E, X} once the app is ready
  let real = null, haltAt = null;                           // real progress (0–1) and the freeze time, from the shell
  const makePlan = (tr) => {
    if (tr < LS - .1) { const E = Math.max(tr, 2.9); return {kEnd: -1, E, X: E + .35}; }
    const kEnd = Math.floor((tr - LS) / P) + 1, E = LS + (kEnd + 1) * P;   // finish this flip, then one more that brings the cover back
    return {kEnd, E, X: E + .5};
  };
  const brand = document.querySelector('.brand'), brandIcon = brand?.querySelector('.logo-mark'), brandName = brand?.querySelector('span');
  const hideBrand = (v) => { for (const el of [brandIcon, brandName]) if (el) el.style.opacity = v ? 0 : ''; };
  hideBrand(true);
  let fly = null, finished = false;

  const render = (t) => {
    const K = (a, b) => inOut(seg(t, a, b));
    // 出现: the base, then the folded strip fades in; it opens round the corners while its creases soften
    const fadeIn = K(.25, .7), rr = RAD * K(.85, 1.9);
    const th = [0, 178 - 88 * K(.4, .9), 182 - 92 * K(.68, 1.18), 178 - 88 * K(.96, 1.46), t < 1.5 ? 182 - 182 * K(1.0, 1.5) : 90 * outCubic(seg(t, 1.5, 1.96))];
    const M0 = 2.08, M1 = 2.32, folding = t < M0;
    floor.setAttribute('d', floorPath(rr));
    floor.setAttribute('transform', `translate(0 ${f(18 * (1 - fadeIn))})`);
    floor.setAttribute('fill', rgb(mixRGB(PALE, gradAt(GR.inside, [640, 700]), K(.7, 1.5))));
    ground.setAttribute('opacity', f(.16 * K(0, .35)));
    if (folding) drawPaper(stripFacets(th, rr), {closed: K(.7, 1.5), lift: -18 * (1 - fadeIn)});
    const leave = 1 - K(1.84, 2.06);
    floorG.setAttribute('opacity', f(K(0, .35) * leave));
    paperBehind.setAttribute('opacity', f(fadeIn * leave)); paperFront.setAttribute('opacity', f(fadeIn * leave));
    floorG.style.display = paperBehind.style.display = paperFront.style.display = folding ? '' : 'none';
    // the smooth version fades in over the paper while the fold curls into the icon's curve
    morph(K(1.64, M1), t >= 1.64 && t < M1, K(1.64, 1.9));
    const art = t >= M1 ? 1 : 0;
    back.setAttribute('opacity', art); bands.setAttribute('opacity', art);

    // the card drops in once three sides stand; while loading it is flipped forward, one card per cycle
    let h = 0, op = 1, rock = 0;
    const cardFrom = 1.36;
    if (t < cardFrom + .28) { const x = seg(t, cardFrom, cardFrom + .28); h = 420 * (1 - x * x); op = seg(t, cardFrom, cardFrom + .07); rock = 6 * deg * Math.sin(x * Math.PI * 2) * (1 - x); }
    else h = 14 * bump(t, cardFrom + .28, cardFrom + .42);
    let theta = 0, squash = 0, flipping = false;
    const endE = plan ? plan.E : haltAt ?? Infinity;
    if (t >= LS && t < endE) {
      const k = Math.floor((t - LS) / P), x = (t - LS - k * P) / (P - .08);
      flipping = true;
      front.color(cardAt(k)); behind.color(plan && k === plan.kEnd ? 'lav' : cardAt(k + 1));
      theta = flipAngle(x);
      squash += .16 * bump(t, LS + k * P + P - .08, LS + k * P + P + .08);
    } else front.color('lav');
    behind.set(pose(), {show: flipping});
    front.set(pose(theta + rock, h), {show: t >= cardFrom, opacity: op, shadeA: .28 * clamp(theta / (70 * deg))});
    let rimP = t >= 2.26 && t < 2.72 ? seg(t, 2.26, 2.72) : -1;
    if (plan && t >= plan.E && plan.kEnd >= 0) { squash += .35 * bump(t, plan.E, plan.E + .2); if (t < plan.E + .5) rimP = seg(t, plan.E, plan.E + .5); }
    squashBands(squash); rim(rimP);

    // the icon starts centred, then glides up to make room for the name
    const press = .75 * bump(t, 1.86, 2.34);
    const drop = (iconBox.parentElement.offsetHeight - iconBox.offsetHeight) / 2, glide = K(2.2, 2.8);
    svg.style.transform = `translateY(${f(drop * (1 - glide))}px) scale(${f(1 + .02 * press)}, ${f(1 - .035 * press)})`;
    letters.forEach((el, i) => {
      const a = 2.3 + i * .04, x = outCubic(seg(t, a, a + .55));
      el.style.opacity = f(seg(t, a, a + .3)); el.style.transform = `translateY(${f((1 - x) * 14)}px)`;
    });
    nameEl.style.letterSpacing = `${f(.12 - .14 * outCubic(seg(t, 2.3, 3.0)))}em`;

    // status: real text from the app; the bar creeps while loading and fills when ready
    statusEl.style.opacity = f(seg(t, 2.8, 3.1) * (plan ? 1 - seg(t, plan.X - .05, plan.X + .2) : 1));
    // a launched load carries on from where the shell's bar stopped (.9) instead of starting over
    const since = Math.max(0, t - (launched ? MADE : 2.8));
    const creep = real ?? (launched ? .9 + .07 * (1 - Math.exp(-since / 2.2)) : .9 * (1 - Math.exp(-since / 2.2)));
    const fillTo = plan ? creep + (1 - creep) * outCubic(seg(t, readyAt, Math.max(readyAt + .3, plan.E))) : creep;
    fillEl.style.width = `${f(100 * fillTo)}%`;
    if (plan && t >= plan.E - .2) textEl.textContent = '准备好了';

    // 进入: icon and name fly to the sidebar brand while the splash's backdrop fades away
    const fx = plan ? inOut(seg(t, plan.X, plan.X + .8)) : 0;
    if (plan && t >= plan.X && !fly) {
      iconBox.style.transform = nameEl.style.transform = '';
      const ok = brandIcon && brandIcon.getBoundingClientRect().width > 0;
      fly = ok ? {icon: [iconBox.getBoundingClientRect(), brandIcon.getBoundingClientRect()], name: [nameEl.getBoundingClientRect(), brandName.getBoundingClientRect()]} : {none: true};
    }
    const move = (el, pair) => {
      if (!pair || fx <= 0) { el.style.transform = ''; return; }
      const [a, z] = pair, s = z.height / a.height;
      el.style.transform = `translate(${f((z.left - a.left) * fx)}px, ${f((z.top - a.top) * fx)}px) scale(${f(1 + (s - 1) * fx)})`;
    };
    if (fly && !fly.none) { move(iconBox, fly.icon); move(nameEl, fly.name); }
    bg.style.opacity = f(1 - fx);
    if (fly && fly.none) root.style.opacity = f(1 - fx);
    if (plan && t >= plan.X + .8) finish();
  };

  const finish = () => {
    if (finished) return; finished = true;
    hideBrand(false); root.remove();
  };

  // ---------- clock ----------
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  // the full folding plays on every load, except the library page the shell opens after playing it
  let t = reduce ? 2.99 : launched ? MADE : 0, last = 0;
  let waiting = [];
  const settle = () => { if (reduce || t >= MADE) { waiting.forEach(done => done()); waiting = []; } };
  const tick = (now) => {
    if (finished) return;
    const dt = last ? Math.min(.05, (now - last) / 1000) : 0; last = now;
    if (!reduce || plan) t += dt;
    if (reduce && plan && t < plan.X) t = plan.X;
    render(t);
    settle();
    requestAnimationFrame(tick);
  };
  // click to skip; not on the shell's start page, which has nothing behind it
  if (!root.hasAttribute('data-shell')) root.addEventListener('click', () => { root.style.transition = 'opacity .25s'; root.style.opacity = 0; setTimeout(finish, 260); });
  window.splash = {
    status(text) { textEl.textContent = text; },
    ready() { if (readyAt === null) { readyAt = t; plan = makePlan(t); } },
    progress(done) { real = clamp(done); },
    // resolves once the icon is made, so the shell never cuts the fold short
    settled() { return new Promise(done => { waiting.push(done); settle(); }); },
    // stop flipping cards (the shell shows an error instead)
    halt() { if (haltAt === null) haltAt = t; }
  };
  render(t);
  requestAnimationFrame(tick);
})();
