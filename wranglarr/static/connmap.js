// System → App connections as a map: each app a box, each link an arrow from
// the app that holds the setting to the app it reaches; both ends when the two
// reach each other. Columns follow the links: Prowlarr/Bazarr/Seerr, the arrs,
// the download clients.
(() => {
  const data = document.getElementById('conn-links'); if (!data) return;
  const { esc, service } = W;
  const $ = id => document.getElementById(id);
  const box = $('cmap'), stage = $('cmap-stage'), detail = $('cmap-detail'), list = $('conn-list');
  const links = JSON.parse(data.textContent || '[]');
  if (!links.length) return;

  // ---- view toggle (map by default, remembered per browser) ----
  const KEY = 'wr.connView', seg = document.querySelector('#conn-panel .seg');
  let view = 'map';
  try { view = localStorage.getItem(KEY) === 'list' ? 'list' : 'map'; } catch { /* no storage */ }
  function setView(v, keep) {
    view = v; box.hidden = v !== 'map'; list.hidden = v !== 'list';
    seg.querySelectorAll('[data-conn-view]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.connView === v)));
    if (keep) try { localStorage.setItem(KEY, v); } catch { /* no storage */ }
    if (v === 'map') draw();
  }
  seg.hidden = false;
  seg.addEventListener('click', e => { const b = e.target.closest('[data-conn-view]'); if (b) setView(b.dataset.connView, true); });

  // ---- the graph ----
  const tone = s => s === 'connected' ? 'ok' : s === 'missing' ? 'missing' : s === 'differs' || s === 'waiting' ? 'warn' : 'bad';
  const RANK = { ok: 0, missing: 1, warn: 2, bad: 3 };
  const nodes = new Map();
  const add = (name, kind) => { if (!nodes.has(name)) nodes.set(name, { name, kind, real: true, links: [] }); return nodes.get(name); };
  links.forEach((l, i) => { l.i = i; add(l.src, l.src_kind).links.push(l); add(l.dst, l.dst_kind).links.push(l); });

  // columns: the longest chain of links leading to an app (capped, in case of a loop)
  const layer = new Map([...nodes.keys()].map(n => [n, 0]));
  for (let p = 0; p < nodes.size; p++) {
    let moved = false;
    for (const l of links) if (layer.get(l.dst) <= layer.get(l.src)) { layer.set(l.dst, layer.get(l.src) + 1); moved = true; }
    if (!moved) break;
  }
  // an app nothing points at sits just before the first app it reaches
  for (const n of nodes.values()) {
    const outs = n.links.filter(l => l.src === n.name), ins = n.links.filter(l => l.dst === n.name);
    if (!ins.length && outs.length) layer.set(n.name, Math.min(...outs.map(l => layer.get(l.dst))) - 1);
  }
  const nl = Math.max(...layer.values()) + 1;
  const cols = Array.from({ length: nl }, () => []);
  for (const n of nodes.values()) { n.layer = layer.get(n.name); cols[n.layer].push(n); }
  // a link that skips a column gets a waypoint in each column it crosses, so it runs between the boxes
  for (const l of links) {
    const a = layer.get(l.src), b = layer.get(l.dst);
    l.chain = [nodes.get(l.src)];
    for (let k = a + 1; k < b; k++) { const d = { layer: k, real: false }; cols[k].push(d); l.chain.push(d); }
    l.chain.push(nodes.get(l.dst));
  }

  // order within columns: each item near the average of its neighbours (a few sweeps both ways)
  const nb = new Map(), pair = (x, y) => { if (!nb.has(x)) nb.set(x, []); nb.get(x).push(y); };
  for (const l of links) for (let j = 1; j < l.chain.length; j++) { pair(l.chain[j - 1], l.chain[j]); pair(l.chain[j], l.chain[j - 1]); }
  const number = c => c.forEach((x, j) => { x.ord = (j + 0.5) / c.length; });
  cols.forEach(number);
  const sweep = (k, from) => {
    for (const x of cols[k]) { const ns = (nb.get(x) || []).filter(y => y.layer === from); x.bc = ns.length ? ns.reduce((s, y) => s + y.ord, 0) / ns.length : x.ord; }
    cols[k].sort((p, q) => p.bc - q.bc || p.ord - q.ord); number(cols[k]);
  };
  for (let it = 0; it < 4; it++) { for (let k = 1; k < nl; k++) sweep(k, k - 1); for (let k = nl - 2; k >= 0; k--) sweep(k, k + 1); }

  // ---- layout: left to right, or top to bottom on a narrow screen ----
  function layout(Wd) {
    const horiz = Wd >= 560, pad = horiz ? 14 : 4, gap = horiz ? 22 : 8, dum = horiz ? 18 : 10, rowGap = 74;
    const nh = horiz ? 46 : 52;
    // box width: one for all across; per row on a phone, so a row of two gets wider boxes than a row of three
    const nwH = Math.max(110, Math.min(180, Wd / nl - 70));
    cols.forEach(c => {
      const real = c.filter(x => x.real).length || 1, dums = c.length - real;
      const w = horiz ? nwH : Math.max(64, Math.min(150, (Wd - 2 * pad - dums * dum) / real - gap));
      c.forEach(x => { x.w = x.real ? w : 0; });
    });
    const size = x => x.real ? (horiz ? nh : x.w) + gap : dum;
    const lens = cols.map(c => c.reduce((s, x) => s + size(x), 0));
    const cross = horiz ? Math.max(...lens) : Wd - 2 * pad;
    cols.forEach((c, k) => {
      let at = pad + (cross - lens[k]) / 2;
      const main = horiz ? (k + 0.5) * Wd / nl : pad + nh / 2 + k * (nh + rowGap);
      for (const x of c) { const s = size(x); x.c = at + s / 2; x.m = main; at += s; }
    });
    const H = horiz ? Math.max(120, cross + 2 * pad) : 2 * pad + nl * nh + (nl - 1) * rowGap;
    const pt = (m, c) => horiz ? { x: m, y: c } : { x: c, y: m };
    // ports: lines leaving a box fan out along its far side, lines arriving along its near side
    const maxStep = horiz ? 8 : 12, ports = new Map();
    for (const n of nodes.values()) {
      const half = (horiz ? n.w : nh) / 2 + 2, span = (horiz ? nh : n.w) - 18;
      for (const dir of ['out', 'in']) {
        const segs = links.filter(l => (dir === 'out' ? l.src : l.dst) === n.name)
          .map(l => ({ l, other: dir === 'out' ? l.chain[1] : l.chain[l.chain.length - 2] }))
          .sort((p, q) => p.other.c - q.other.c);
        const step = segs.length > 1 ? Math.min(maxStep, span / (segs.length - 1)) : 0;
        segs.forEach((s, j) => ports.set(`${dir}${s.l.i}`, pt(n.m + (dir === 'out' ? half : -half), n.c + (j - (segs.length - 1) / 2) * step)));
      }
    }
    for (const l of links) {
      // through a skipped column: straight across the width of its boxes, so it clears their corners
      const across = horiz ? nwH / 2 : nh / 2;
      const p = [ports.get(`out${l.i}`), ...l.chain.slice(1, -1).flatMap(d => [pt(d.m - across, d.c), pt(d.m + across, d.c)]), ports.get(`in${l.i}`)];
      let d = `M${p[0].x.toFixed(1)} ${p[0].y.toFixed(1)}`;
      for (let j = 1; j < p.length; j++) {
        const a = p[j - 1], b = p[j];
        d += horiz ? ` C${((a.x + b.x) / 2).toFixed(1)} ${a.y.toFixed(1)} ${((a.x + b.x) / 2).toFixed(1)} ${b.y.toFixed(1)} ${b.x.toFixed(1)} ${b.y.toFixed(1)}`
                   : ` C${a.x.toFixed(1)} ${((a.y + b.y) / 2).toFixed(1)} ${b.x.toFixed(1)} ${((a.y + b.y) / 2).toFixed(1)} ${b.x.toFixed(1)} ${b.y.toFixed(1)}`;
      }
      l.d = d;
    }
    const rects = [...nodes.values()].map(n => { const c = pt(n.m, n.c); return { n, x: c.x - n.w / 2, y: c.y - nh / 2 }; });
    return { horiz, H, nh, rects };
  }

  // ---- drawing ----
  const logo = (name, kind, cls = 'cm-logo') =>
    `<img class="${cls}" src="/api/apps/${encodeURIComponent(name)}/logo" alt="" data-fallback="${esc(service(kind).icon || '')}">`;
  const arrow = l => l.both ? '⇄' : '→';
  const head = t => `<marker id="cm-${t}" class="cm-head t-${t}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="9" markerHeight="9" markerUnits="userSpaceOnUse" orient="auto-start-reverse"><path d="M0 0.5L10 5L0 9.5z"/></marker>`;
  let lastW = 0;

  function draw() {
    const Wd = stage.clientWidth; if (!Wd) return;
    lastW = Wd;
    const L = layout(Wd);
    stage.style.height = `${L.H}px`;
    stage.classList.toggle('vert', !L.horiz);
    const edges = links.map(l => {
      const t = l.both && tone(l.state) === 'ok' ? 'two' : tone(l.state);    // healthy two-way links in blue
      return `<g class="cm-edge t-${t}" data-i="${l.i}"><title>${esc(`${l.src} ${arrow(l)} ${l.dst} (${l.what}): ${l.state}`)}</title>
        <path class="cm-hit" d="${l.d}"/><path class="cm-line" d="${l.d}" marker-end="url(#cm-${t})"${l.both ? ` marker-start="url(#cm-${t})"` : ''}/></g>`;
    }).join('');
    const boxes = L.rects.map(({ n, x, y }) => {
      const worst = n.links.reduce((w, l) => RANK[tone(l.state)] > RANK[w] ? tone(l.state) : w, 'ok');
      const label = service(n.kind).label || n.kind, sub = label.toLowerCase() !== n.name.toLowerCase() ? label : '';
      return `<button type="button" class="cm-node" data-node="${esc(n.name)}" aria-pressed="false"
        aria-label="${esc(`${n.name}: ${n.links.length} connection${n.links.length === 1 ? '' : 's'}`)}"
        style="left:${x.toFixed(1)}px;top:${y.toFixed(1)}px;width:${n.w.toFixed(1)}px;height:${L.nh}px">
        ${logo(n.name, n.kind)}<span class="cm-txt"><span class="cm-name">${esc(n.name)}</span>${sub ? `<span class="cm-sub">${esc(sub)}</span>` : ''}</span>
        ${worst !== 'ok' ? `<i class="cm-dot t-${worst}" title="a connection needs attention"></i>` : ''}</button>`;
    }).join('');
    stage.innerHTML = `<svg class="cmap-svg" width="${Wd}" height="${L.H}" aria-hidden="true"><defs>${['ok', 'two', 'warn', 'missing', 'bad'].map(head).join('')}</defs>${edges}</svg>${boxes}`;
    paint();
  }

  // ---- hover and selection ----
  let sel = null, hov = null;
  const same = (a, b) => (a && a.node) === (b && b.node) && (a && a.link) === (b && b.link);
  const pick = el => {
    const n = el.closest('.cm-node'); if (n) return { node: n.dataset.node };
    const g = el.closest('.cm-edge'); if (g) return { link: +g.dataset.i };
    return null;
  };
  function paint() {
    const f = hov || sel;
    let on = null;
    if (f && f.node != null) { const n = nodes.get(f.node); on = { l: new Set(n.links.map(l => l.i)), n: new Set(n.links.flatMap(l => [l.src, l.dst]).concat(n.name)) }; }
    else if (f) { const l = links[f.link]; on = { l: new Set([l.i]), n: new Set([l.src, l.dst]) }; }
    stage.classList.toggle('focusing', !!on);
    stage.querySelectorAll('.cm-edge').forEach(g => {
      const lit = !!on && on.l.has(+g.dataset.i);
      g.classList.toggle('on', lit);
      if (lit) g.parentNode.appendChild(g);         // lit lines on top
    });
    stage.querySelectorAll('.cm-node').forEach(b => {
      b.classList.toggle('on', !!on && on.n.has(b.dataset.node));
      b.setAttribute('aria-pressed', String(!!sel && sel.node === b.dataset.node));
    });
  }

  const pill = s => s === 'connected' ? '<span class="pill pill-ok">connected</span>'
    : s === 'missing' ? '<span class="pill">not connected</span>'
    : `<span class="pill ${s === 'differs' || s === 'waiting' ? 'pill-partial' : 'pill-failed'}">${esc(s)}</span>`;
  const says = l => l.what === 'download client' ? `${l.src} sends what it grabs to ${l.dst}`
    : l.both ? `${l.src} puts its indexers into ${l.dst}, and ${l.dst} searches through ${l.src}`
    : `${l.src} is set up to use ${l.dst}`;
  const row = l => `<tr><td>${logo(l.src, l.src_kind, 'mini-logo')} ${esc(l.src)} <span class="cm-arrow">${arrow(l)}</span>
      ${logo(l.dst, l.dst_kind, 'mini-logo')} ${esc(l.dst)} <span class="muted small">${esc(l.what)}</span>
      <div class="muted small">${esc(says(l))}</div></td>
    <td><code>${esc(l.target || '–')}</code></td>
    <td>${pill(l.state)}${l.detail ? ` <span class="muted small">${esc(l.detail)}</span>` : ''}</td>
    <td>${['connected', 'error', 'waiting'].includes(l.state) ? '' : `<button class="btn ghost small" data-connect="${esc(l.key)}" data-label="${esc(`${l.dst} to ${l.src}`)}">connect</button>`}</td></tr>`;
  function showDetail() {
    if (!sel) {
      const bad = links.filter(l => l.state !== 'connected').length;
      detail.innerHTML = `<p class="muted small">${links.length} connections between ${nodes.size} apps${bad ? ` · <b>${bad} need${bad === 1 ? 's' : ''} a look</b>` : ', all connected'}.
        Click an app or a line for the details.</p>`;
      return;
    }
    const ls = sel.node != null ? nodes.get(sel.node).links : [links[sel.link]];
    const title = sel.node != null ? `<div class="cm-title">${logo(sel.node, nodes.get(sel.node).kind, 'mini-logo')} <b>${esc(sel.node)}</b>
      <span class="muted small">${ls.length} connection${ls.length === 1 ? '' : 's'}</span></div>` : '';
    detail.innerHTML = `${title}<table class="compact"><tbody>${ls.map(row).join('')}</tbody></table>`;
  }
  const select = s => { sel = s && !same(s, sel) ? s : null; paint(); showDetail(); };

  stage.addEventListener('pointerover', e => {
    if (e.pointerType !== 'mouse') return;
    const h = pick(e.target); if (same(h, hov)) return;
    hov = h; paint();
  });
  stage.addEventListener('pointerleave', () => { if (hov) { hov = null; paint(); } });
  stage.addEventListener('click', e => select(pick(e.target)));
  box.addEventListener('keydown', e => { if (e.key === 'Escape' && sel) select(null); });
  new ResizeObserver(() => { if (!box.hidden && stage.clientWidth !== lastW) draw(); }).observe(stage);

  showDetail();
  setView(view, false);
})();
