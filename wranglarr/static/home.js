// Home: a card per app (key stats on it, the rest on hover), and under it its lists:
// upcoming / missing for the arrs, the queue for a download client, and so on.
(() => {
  const { api, esc, safeUrl, fmtBytes, fmtN, toast, ask, service, LABELS } = W;
  // the setup that matters most per app, shown with the details
  const SCOPE = [['indexer', 'indexers'], ['downloadclient', 'download clients'], ['applications', 'apps'], ['providers', 'providers']];
  const home = document.getElementById('home');
  const cols = () => [...document.querySelectorAll('.app-col')];
  const col = name => document.querySelector(`.app-col[data-app="${CSS.escape(name)}"]`);

  const stat = (n, label) => `<span class="stat"><b>${esc(n)}</b> ${esc(label)}</span>`;

  function render(el, a) {
    const dot = el.querySelector('.dot'), ver = el.querySelector('.poster-ver'), face = el.querySelector('.note-stats');
    const cw = el.querySelector('.card-warn');
    // the details sit twice: over the poster / under the card on hover, and below it with "always"
    const fill = (sel, html) => el.querySelectorAll(sel).forEach(x => { x.innerHTML = html; });
    dot.className = 'dot ' + (a.online ? 'online' : 'offline');
    ver.textContent = a.online ? (a.version || '') : 'offline';
    const health = a.health || [];
    cw.hidden = !health.length;
    cw.innerHTML = W.icon('warning') + (health.length > 1 ? ` ${health.length}` : '');
    cw.title = health.map(h => h.message).join('\n');
    // health warnings: in plain sight above the app's list, not behind a hover
    const box = el.querySelector('.app-warn'), base = el.dataset.url.replace(/\/$/, '');
    box.hidden = !health.length;
    const page = service(el.dataset.kind).status_page;
    const status = base && page && safeUrl(base) ? `<a href="${esc(base + page)}" target="_blank" rel="noopener">open ›</a>` : '';
    box.innerHTML = health.length ? `<div class="app-warn-head">${W.icon('warning')} ${health.length === 1 ? 'Health warning' : health.length + ' health warnings'}${status}</div>`
      + `<ul>${health.map(h => `<li${h.type === 'error' ? ' class="err"' : ''}>${esc(h.message)}</li>`).join('')}</ul>` : '';
    if (!a.online) {
      face.className = 'note-stats error'; face.textContent = a.error || 'unreachable'; face.title = a.error || '';
      fill('.more-stats', `<span class="error">${esc(a.error || 'unreachable')}</span>`); return;
    }
    const c = a.collection || {}, s = a.scope || {};
    const counts = Object.entries(c).filter(([k]) => k !== 'size_on_disk').map(([k, v]) => stat(fmtN(v), LABELS[k] || k));
    const disk = [];
    if ('size_on_disk' in c) disk.push(stat(fmtBytes(c.size_on_disk), 'on disk'));
    if (s.root_free_space) disk.push(stat(fmtBytes(s.root_free_space), 'free'));
    const shown = new Set(Object.keys(c).map(k => LABELS[k] || k));
    const setup = SCOPE.filter(([k, label]) => k in s && !shown.has(label)).map(([k, label]) => stat(s[k], label));
    face.className = 'note-stats'; face.title = '';
    const line = l => `<span class="more-line">${l.join('')}</span>`;
    if (el.classList.contains('is-poster')) {      // the poster has room: what it holds and the disk on it; the setup on hover
      face.innerHTML = [counts, disk].filter(l => l.length).map(line).join('') || 'no stats';
      fill('.more-stats', setup.length ? line(setup) : '');
    } else {                                       // a card: the three that matter; the rest on hover
      face.innerHTML = counts.slice(0, 3).join('') || 'no stats';
      fill('.more-stats', [counts.slice(3), disk, setup].filter(l => l.length).map(line).join(''));
    }
  }

  async function loadSuite(name, refresh) {
    if (!cols().some(c => c.dataset.suite === name)) return;
    try {
      const o = await api('GET', `/api/suites/${encodeURIComponent(name)}/overview${refresh ? '?refresh=1' : ''}`);
      for (const a of o.apps) { const el = col(a.name); if (el) render(el, a); }
    } catch (e) { toast(`${name}: ${e.message}`, true); }
  }
  async function load(refresh) {
    const suites = [...new Set(cols().map(c => c.dataset.suite))];
    await Promise.all(suites.map(name => loadSuite(name, refresh)));
  }

  // logos that fail to load: app.js swaps in the kind's shipped one
  const fallback = () => {};
  fallback();

  // ---- times ----
  const DAY = 86400000;
  const midnight = d => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const hm = d => d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  function when(iso, ahead, day) {     // ahead: "Today 20:00", "Thu 29"; behind: "yesterday", "3 d ago", "2016"
    if (!iso) return '';
    let d = new Date(iso); if (isNaN(d)) return '';
    if (day) d = new Date(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate());   // a release date: that day, wherever you are
    const days = Math.round((midnight(d) - midnight(new Date())) / DAY);
    const t = day ? '' : ' ' + hm(d);
    if (ahead) {
      if (days === 0) return 'Today' + t;
      if (days === 1) return 'Tomorrow' + t;
      if (days > 1 && days < 7) return d.toLocaleDateString([], { weekday: 'short' }) + t;
      return d.toLocaleDateString([], { day: 'numeric', month: 'short' });
    }
    if (days === 0) return 'today';
    if (days === -1) return 'yesterday';
    if (days > -14) return `${-days} d ago`;
    if (days > -60) return `${Math.round(-days / 7)} wk ago`;
    if (days > -365) return d.toLocaleDateString([], { day: 'numeric', month: 'short' });
    return String(d.getFullYear());
  }

  // ---- the lists under the arrs, Bazarr, Prowlarr, Seerr ----
  const EMPTY = { indexers: () => 'No indexers yet.', requests: () => 'No requests yet.' };
  function rowsHtml(el, l, empty, ahead) {
    const base = el.dataset.url.replace(/\/$/, '');
    if (l.error) return `<li class="fi-empty error small">${esc(l.error)}</li>`;
    if (!l.items.length) return `<li class="fi-empty muted small">${esc(empty)}${l.key === 'indexers' ? ` <a href="${esc(W.url('/indexers'))}">Add some</a>` : ''}</li>`;
    return l.items.map(i => {
      const tag = i.tag === 'downloaded' ? '<span class="fi-tag ok-text" title="downloaded">✓</span>'
        : i.tag ? `<span class="fi-tag">${esc(i.tag)}</span>` : '';
      const inner = `<span class="fi-when">${esc(when(i.when, ahead, i.day))}</span>
        <span class="fi-main"><span class="fi-title">${esc(i.title)}</span>${i.sub ? `<span class="fi-sub">${esc(i.sub)}</span>` : ''}</span>${tag}`;
      const href = base && i.path ? safeUrl(base + i.path) : '';      // the item's page in the app, http(s) only
      return `<li class="fi${i.state ? ' st-' + esc(i.state) : ''}">${href ? `<a href="${esc(href)}" target="_blank" rel="noopener">${inner}</a>` : `<span class="fi-row">${inner}</span>`}</li>`;
    }).join('');
  }
  const firstOf = l => l.total > l.items.length ? `<span class="feed-more muted small" title="change it under System → Dashboard">first ${l.items.length}</span>` : '';

  // Bazarr, Prowlarr, Seerr: one list each
  function feedHtml(el, l, st) {
    const empty = l.empty || (el.dataset.kind === 'bazarr' ? 'No subtitles wanted.' : (EMPTY[l.key] || (() => 'Nothing.'))(st.days));
    return `<section class="feed" data-key="${esc(l.key)}">
      <div class="feed-head"><span class="feed-title">${esc(l.title)}</span>${l.note ? `<span class="muted small">${esc(l.note)}</span>` : ''}
        ${firstOf(l)}<span class="feed-count">${l.error ? '' : fmtN(l.total || 0)}</span></div>
      <ul class="feed-list">${rowsHtml(el, l, empty, false)}</ul></section>`;
  }

  // an arr: one table, what's coming up (soonest first), then what went missing lately (newest first)
  function scheduleHtml(el, up, mi, st) {
    const base = el.dataset.url.replace(/\/$/, '');
    const parts = [], counts = [];
    if (up) {
      counts.push(`${fmtN(up.total || 0)} coming`);
      const days = up.days || st.days;       // films, albums and books look further ahead than episodes
      parts.push(`<li class="fi-group"><span>Coming up</span><span class="muted">${esc(up.note || `next ${days} days`)}</span>${up.next ? '' : firstOf(up)}</li>`
        + rowsHtml(el, up, `Nothing in the next ${days} days.`, true));
    }
    if (mi) {
      counts.push(`${fmtN(mi.total || 0)} missing`);
      const span = st.missing_days ? `last ${st.missing_days} days` : 'all';
      const all = mi.all > mi.total && base && safeUrl(base + mi.path) ? `<a class="feed-more small" href="${esc(safeUrl(base + mi.path))}" target="_blank" rel="noopener" title="the arr's Wanted → Missing">${fmtN(mi.all)} in all ›</a>`
        : firstOf(mi);
      parts.push(`<li class="fi-group"><span>Missing</span><span class="muted">${esc(span)}</span>${all}</li>`
        + rowsHtml(el, mi, st.missing_days ? `Nothing missing from the last ${st.missing_days} days.` : 'Nothing missing.', false));
    }
    return `<section class="feed schedule">
      <div class="feed-head"><span class="feed-title">${up && mi ? 'Coming up & missing' : up ? 'Coming up' : 'Missing'}</span>
        <span class="feed-count">${counts.join(' · ')}</span></div>
      <ul class="feed-list">${parts.join('')}</ul></section>`;
  }

  function appFeeds(el, f, st) {
    const up = f.lists.find(l => l.key === 'upcoming'), mi = el.dataset.kind === 'bazarr' ? null : f.lists.find(l => l.key === 'missing');
    const rest = f.lists.filter(l => l !== up && l !== mi);
    return (up || mi ? scheduleHtml(el, up, mi, st) : '') + rest.map(l => feedHtml(el, l, st)).join('');
  }

  let feedTimer = 0;
  async function loadFeeds(refresh) {
    clearTimeout(feedTimer);
    const boxes = document.querySelectorAll('[data-feed]');
    if (!boxes.length) return;
    let r;
    try { r = await api('GET', '/api/home/feeds' + (refresh ? '?refresh=1' : '')); }
    catch (e) { boxes.forEach(b => { b.innerHTML = `<p class="error small">${esc(e.message)}</p>`; }); feedTimer = setTimeout(loadFeeds, 60000); return; }
    for (const b of boxes) {
      const el = b.closest('.app-col'), f = r.apps[el.dataset.app];
      b.innerHTML = !f ? '' : f.error ? `<p class="error small">${esc(f.error)}</p>` : appFeeds(el, f, r.settings);
    }
    if (!document.hidden) feedTimer = setTimeout(loadFeeds, 120000);
  }

  // ---- Links: where each app is (a name or path on the proxy, or direct) and where its card opens ----
  const panel = document.getElementById('links-panel'), form = document.getElementById('links-form');
  const D = JSON.parse(document.getElementById('links-data').textContent);
  const rowsEl = document.getElementById('links-rows');
  const AT = { subdomain: 'Subdomain', path: 'wranglarr path', domain: 'The domain itself', direct: 'Direct IP', other: 'Other address' };
  const portTag = how => !how || how === 'published port' ? '' : how === 'through wranglarr-gate' ? 'via the gate'
    : how.startsWith('guessed') ? 'guessed' : how.startsWith('not published') ? 'not published' : how;
  function rowHtml(a) {
    const me = a.name === 'wranglarr';
    const ats = me ? ['subdomain', 'domain', 'path', 'direct'] : ['subdomain', 'path', 'direct', 'other'];
    const cur = a.custom ? 'other' : a.at;
    const opts = ats.map(v => `<option value="${v}"${v === cur ? ' selected' : ''}${v === 'path' && a.own_name ? ' disabled title="needs a name of its own"' : ''}>${me && v === 'path' ? 'Path on the domain' : AT[v]}</option>`).join('');
    const now = a.now ? `<div class="hint">card now: ${a.now.url ? `<a href="${esc(a.now.url)}" target="_blank" rel="noopener">${esc(a.now.url)}</a>` : 'no link'} · ${esc(a.now.note)}</div>` : '';
    const login = a.gated ? 'behind wranglarr' : D.tunnel && !me ? 'own login' : '';
    return `<tr data-row="${esc(a.name)}">
      <td>${esc(a.name)}${login ? `<div class="muted small">${login}</div>` : ''}</td>
      <td title="${esc(a.port_how || '')}">${a.port ? `<code>${a.port}</code>` : '<span class="muted">—</span>'} <span class="muted small">${esc(portTag(a.port_how))}</span></td>
      <td><select name="at">${opts}</select></td>
      <td><label class="inline small links-local"><input type="checkbox" name="local"${a.local ? ' checked' : ''}> local only</label></td>
      <td><span class="links-addr"><span data-pre></span><input name="sub" value="${esc(a.sub)}" placeholder="${esc(a.name)}" spellcheck="false" size="${Math.max(a.name.length, 4)}"><span data-post></span>
        <input name="custom" value="${esc(a.custom || '')}" placeholder="http://192.168.1.20:8080 or https://sab.example.com" spellcheck="false"></span>${now}</td></tr>`;
  }
  const rows = () => [...rowsEl.querySelectorAll('tr[data-row]')];
  const app = name => D.apps.find(a => a.name === name);
  function preview() {
    const el = form.elements;
    const domain = el.domain.value.trim().toLowerCase().replace(/\.$/, '') || D.cookie_domain || 'example.com';
    const host = el.host.value.trim() || D.this_machine, scheme = el.scheme.value;
    const w = rowsEl.querySelector('tr[data-row="wranglarr"]');
    const wat = w.querySelector('[name=at]').value, wsub = w.querySelector('[name=sub]').value.trim() || 'wranglarr';
    const wfqdn = ['domain', 'path'].includes(wat) ? domain : wat === 'direct' ? '' : `${wsub}.${domain}`;
    for (const r of rows()) {
      const a = app(r.dataset.row), sel = r.querySelector('[name=at]');
      let at = sel.value;
      const [pre, post, sub, custom] = ['[data-pre]', '[data-post]', '[name=sub]', '[name=custom]'].map(q => r.querySelector(q));
      const local = r.querySelector('[name=local]');
      local.disabled = at === 'direct' || at === 'other';
      local.closest('label').style.visibility = local.disabled ? 'hidden' : '';
      let p = '', q = '', note = '';
      if (at === 'path' && !wfqdn) { at = 'subdomain'; note = ' (wranglarr is Direct IP: no name to go under)'; }
      if (at === 'subdomain') { p = 'https://'; q = `.${domain}/${note}`; }
      else if (at === 'path') { p = `https://${wfqdn}/`; q = '/'; }
      else if (at === 'domain') { p = `https://${domain}/`; }
      else if (at === 'direct') {
        const port = a.port;
        p = !host ? 'put this machine\'s address above' : port ? `${scheme}://${host.includes(':') ? `[${host}]` : host}:${port}${a.base || ''}/` : 'not published on this machine: use Other address';
      }
      pre.textContent = p; post.textContent = q;
      sub.hidden = !(at === 'subdomain' || at === 'path');
      custom.hidden = at !== 'other';
    }
    const wpath = wat === 'path' ? `/${wsub}` : (D.url_base || '');
    linkWarnings(domain, wfqdn, wpath);
  }
  // AuthTunnel: the gate sends you to wranglarr's page to sign in, and the cookie has to reach each app's address
  function linkWarnings(domain, wfqdn, wpath) {
    const under = (h, d) => !!h && !!d && (h === d || h.endsWith('.' + d));
    const at = r => r.querySelector('[name=at]').value;
    const proxied = rows().filter(r => app(r.dataset.row).gated && ['subdomain', 'path'].includes(at(r)));
    const named = proxied.filter(r => at(r) === 'subdomain' || !wfqdn).map(r => r.dataset.row);
    const list = rs => rs.map(r => typeof r === 'string' ? r : r.dataset.row).join(', ');
    const cd = D.cookie_domain, warn = [];
    const wr = rows().find(r => r.dataset.row === 'wranglarr');
    if (wr && at(wr) === 'path' && (D.url_base || '') !== wpath) warn.push(`Set WRANGLARR_URL_BASE=${wpath} in .env and recreate wranglarr. The proxy must pass the path through unchanged.`);
    let loginAddress = '';
    try { loginAddress = D.login_url ? new URL(D.login_url).href.replace(/\/$/, '') : ''; } catch (e) { /* shown as it is */ }
    if (D.tunnel && proxied.length) {
      if (!wfqdn) warn.push(`AuthTunnel signs you in on wranglarr's page: put wranglarr on the proxy too, or ${list(proxied)} can't sign you in there.`);
      if (cd && !under(domain, cd)) warn.push(`The sign-in cookie covers ${cd} and the names under it, not ${domain}: use ${cd} or a name under it here, or change the cookie domain (wranglarr-setup --auth).`);
      if (!cd && named.length) warn.push(`AuthTunnel has no cookie domain, so an app at a name of its own (${list(named)}) can't share your sign-in: put it at a wranglarr path, or set the cookie domain (wranglarr-setup --auth).`);
      if (wfqdn && !D.login_url) warn.push(`AuthTunnel doesn't know wranglarr's address on the proxy: set it to https://${wfqdn}${wpath} (wranglarr-setup --auth, then ./scripts/init.sh --auth).`);
      else if (wfqdn && loginAddress && loginAddress !== `https://${wfqdn}${wpath}`) warn.push(`AuthTunnel sends you to sign in at ${D.login_url}, but wranglarr is at https://${wfqdn}${wpath} here: change one of them (wranglarr-setup --auth).`);
    }
    document.getElementById('links-warn').innerHTML = warn.map(w => `<div class="check warn">${esc(w)}</div>`).join('');
  }
  function fillRows() { rowsEl.innerHTML = D.apps.map(rowHtml).join(''); preview(); }
  fillRows();
  const toggle = open => { panel.hidden = !open;
    document.querySelectorAll('[data-links-edit]').forEach(b => b.setAttribute('aria-expanded', String(open)));
    if (open) panel.scrollIntoView({ block: 'start', behavior: 'smooth' }); };
  if (location.hash === '#links') toggle(true);
  form.addEventListener('input', preview);
  form.addEventListener('change', preview);
  form.addEventListener('submit', async e => {
    e.preventDefault();
    const el = form.elements, apps = {}, custom = {};
    for (const r of rows()) {
      const name = r.dataset.row, at = r.querySelector('[name=at]').value;
      apps[name] = { at: at === 'other' ? 'direct' : at, local: r.querySelector('[name=local]').checked, sub: r.querySelector('[name=sub]').value.trim() };
      if (at === 'other') {
        const u = r.querySelector('[name=custom]').value.trim();
        if (!u) { toast(`${name}: fill in its address, or pick another place`, true); return; }
        custom[name] = u;
      }
    }
    try {
      await api('PUT', '/api/links/configuration', {
        proxy: { domain: el.domain.value, lan: el.lan.value, apps },
        links: { mode: 'port', scheme: el.scheme.value, host: el.host.value, pattern: '', apps: custom }
      });
      toast('saved'); location.reload();
    } catch (err) { toast(err.message, true); }
  });
  document.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.matches('[data-links-edit]')) toggle(panel.hidden);
    else if (b.matches('[data-links-cancel]')) { form.reset(); fillRows(); toggle(false); }
    else if (b.matches('[data-links-reset]')) {
      ask({ title: 'Forget the links?', body: 'This machine\'s address and the other addresses set here go; where each app is on the proxy stays.', ok: 'Reset' })
        .then(yes => yes && api('DELETE', '/api/links').then(() => location.reload()).catch(err => toast(err.message, true)));
    } else if (b.matches('[data-home-refresh]')) { b.disabled = true; load(true).finally(() => b.disabled = false); }
  });

  // ---- Logins: how the apps' logins are set up (read only; init sets them) ----
  const lpanel = document.getElementById('logins-panel');
  const METHOD = { own: 'own sign-in', forms: 'login page', form: 'login page', basic: 'basic login', external: 'none of its own', none: 'no login' };
  async function loadLogins() {
    let st;
    try { st = await api('GET', '/api/app-logins'); } catch (e) { toast('logins: ' + e.message, true); return; }
    const tunnel = st.mode === 'tunnel';
    const sum = [];
    if (st.mode === 'apps') sum.push(`user <b>${esc(st.username || '?')}</b>, the arrs ask for it ${esc(st.required_text)}`);
    if (tunnel) sum.push(`sign-in cookie for ${st.cookie_domain ? '<b>' + esc(st.cookie_domain) + '</b> and its subdomains' : 'this address only'}` +
      (st.login_url ? `, login page at <b>${esc(st.login_url)}</b>` : ''));
    if (tunnel && !st.wranglarr_auth) sum.push('<span class="error">wranglarr\'s own sign-in is off, so the gate lets everyone in</span>');
    if (st.last_run) sum.push(`set by init on ${esc((st.last_run.at || '').replace('T', ' ').slice(0, 16))} UTC`);
    else if (st.mode !== 'manual') sum.push('<span class="warn-text">init hasn\'t applied it yet: run ./scripts/init.sh --auth</span>');
    document.getElementById('logins-summary').innerHTML = sum.join(' · ');
    document.getElementById('logins-apps').innerHTML = st.apps.map(r => {
      const now = r.error ? `<span class="error">${esc(r.error)}</span>`
        : r.gate || (tunnel && !r.login) ? W.icon('shield') + ' behind wranglarr' + (r.login ? ` <span class="muted">(${esc(r.username)}, for the arrs)</span>` : '')
        : r.login ? `${esc(METHOD[r.method] || r.method)} · <b>${esc(r.username || '?')}</b>${r.required === 'disabledForLocalAddresses' ? ' · not asked locally' : ''}`
        : `<span class="warn-text">${esc(METHOD[r.method] || 'no login')}</span>`;
      return `<tr><td>${esc(r.app)}</td><td class="small">${now}</td><td class="small warn-text">${esc(r.warn || '')}</td></tr>`;
    }).join('');
    for (const r of st.apps) {                       // the lock line in each card's details
      for (const el of document.querySelectorAll(`.app-col[data-app="${CSS.escape(r.app)}"] .note-login`)) {
      const behind = !r.error && tunnel && (r.gate || !r.login) && !r.warn;
      el.className = 'note-login ' + (r.error ? 'error' : r.warn ? 'warn-text' : 'muted');
      el.innerHTML = r.error ? W.icon('key') + ' ?' : behind ? W.icon('shield') + ' behind wranglarr'
        : r.login ? `${W.icon('lock')} ${esc(r.username || METHOD[r.method])}` : W.icon('unlock') + ' no login';
      el.title = r.error || r.warn || (behind ? 'wranglarr-gate asks for your wranglarr sign-in' : r.login ? `${METHOD[r.method] || r.method}, user ${r.username}` : '');
      }
    }
  }
  const ltoggle = open => { lpanel.hidden = !open;
    document.querySelectorAll('[data-logins-edit]').forEach(b => b.setAttribute('aria-expanded', String(open)));
    if (open) lpanel.scrollIntoView({ block: 'start', behavior: 'smooth' }); };
  document.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.matches('[data-logins-edit]')) ltoggle(lpanel.hidden);
    else if (b.matches('[data-logins-cancel]')) ltoggle(false);
  });

  // ---- each download client's queue, under its card; the arr that sent a download shows as its logo ----
  if (home.dataset.showQueue === 'on' && document.querySelector('[data-queue]')) {
    const STATE = { downloading: 'downloading', processing: 'unpacking', checking: 'checking', metadata: 'getting metadata',
      stalled: 'stalled', error: 'error', queued: 'queued', paused: 'paused' };
    const fmtEta = s => { if (!s) return ''; if (s < 60) return `${s}s`; if (s < 3600) return `${Math.round(s / 60)} min`;
      if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.round(s % 3600 / 60)}m`; return `${Math.round(s / 86400)} d`; };
    const fmtSpeed = b => b ? `${fmtBytes(b)}/s` : '';
    let timer = 0;

    function row(d) {
      const pct = Math.floor(d.progress * 1000) / 10;
      const who = d.arr ? `<img class="dl-arr" src="${esc(W.url(`/api/apps/${encodeURIComponent(d.arr.name)}/logo`))}" alt="" title="sent by ${esc(d.arr.name)}" data-fallback="${esc(service(d.arr.kind).icon || '')}">`
        : `<span class="dl-arr dl-none" title="${esc(d.category ? `category "${d.category}": no arr` : 'no category: added by hand')}">?</span>`;
      const meta = [d.arr ? d.arr.name : d.category ? `category ${d.category}` : 'added by hand', STATE[d.state] || d.state,
        d.speed ? fmtSpeed(d.speed) : '', d.eta ? fmtEta(d.eta) + ' left' : '',
        d.size ? (d.progress < 1 ? `${fmtBytes(d.done)} of ${fmtBytes(d.size)}` : fmtBytes(d.size)) : ''].filter(Boolean);
      return `<li class="dl-item st-${esc(d.state)}" title="${esc(`${d.name}${d.category ? '\ncategory ' + d.category : ''} · ${d.state_text || d.state}`)}">
        ${who}
        <span class="dl-main"><span class="dl-name">${esc(d.name)}</span>
          <span class="dl-bar"><span style="width:${pct}%"></span></span>
          <span class="dl-meta small">${meta.map(esc).join(' · ')}</span></span>
        <span class="dl-pct">${pct >= 100 ? '' : pct.toFixed(pct < 10 ? 1 : 0) + '%'}</span></li>`;
    }

    async function loadDl(refresh) {
      clearTimeout(timer);
      let r;
      try { r = await api('GET', '/api/downloads' + (refresh ? '?refresh=1' : '')); }
      catch (e) {
        document.querySelectorAll('[data-queue]').forEach(b => { b.innerHTML = `<p class="error small">${esc(e.message)}</p>`; });
        timer = setTimeout(loadDl, 30000); return;
      }
      for (const box of document.querySelectorAll('[data-queue]')) {
        const el = box.closest('.app-col'), name = el.dataset.app;
        const c = r.clients.find(x => x.name === name) || { ok: true, count: 0, speed: 0 };
        const items = r.items.filter(d => d.client === name);
        const active = items.filter(d => d.state === 'downloading').length;
        const sp = el.querySelector('.card-speed');
        sp.hidden = !c.speed; sp.textContent = c.speed ? '↓ ' + fmtSpeed(c.speed) : '';
        const note = items.length ? `${active ? active + ' downloading' : 'nothing moving'}${c.speed ? ' · ' + fmtSpeed(c.speed) : ''}` : '';
        box.innerHTML = `<section class="feed" data-key="queue">
          <div class="feed-head"><span class="feed-title">Queue</span><span class="muted small">${esc(note)}</span>
            <span class="feed-count">${c.ok ? fmtN(items.length) : ''}</span></div>
          <ul class="feed-list dl-list">${!c.ok ? `<li class="fi-empty error small">${esc(c.error || 'unreachable')}</li>`
            : items.length ? items.map(row).join('') : '<li class="fi-empty muted small">Nothing downloading.</li>'}</ul></section>`;
        fallback(box);
      }
      // faster while something moves; nothing while the tab is hidden
      if (!document.hidden) timer = setTimeout(loadDl, r.items.some(d => d.state === 'downloading' || d.state === 'processing') ? 5000 : 15000);
    }
    document.addEventListener('visibilitychange', () => { if (!document.hidden) loadDl(); else clearTimeout(timer); });
    document.addEventListener('click', e => { if (e.target.closest('[data-home-refresh]')) loadDl(true); });
    loadDl(false);
  }

  document.addEventListener('visibilitychange', () => { if (!document.hidden) loadFeeds(); else clearTimeout(feedTimer); });
  document.addEventListener('click', e => { if (e.target.closest('[data-home-refresh]')) loadFeeds(true); });
  loadFeeds(false);
  // live status: a suite the arrs said changed (its overview is fresh on the server already)
  document.addEventListener('wranglarr:event', e => {
    const d = e.detail;
    if (d.type === 'overview') loadSuite(d.suite);
    else if (d.type === 'resync') load(false);
  });
  load(false);
  loadLogins();
})();
