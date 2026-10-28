const W = (() => {
  const fmtBytes = n => { if (!n && n !== 0) return '?'; const u = ['B','KB','MB','GB','TB','PB']; let i = 0; n = +n;
    while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; } return (i ? n.toFixed(1) : n) + ' ' + u[i]; };
  const fmtN = n => (+n).toLocaleString();
  const esc = s => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  // a URL from the server or an upstream app, for an href: only http(s) (absolute or relative to this page), else ''
  const safeUrl = u => { if (u == null || u === '') return '';
    try { return ['http:', 'https:'].includes(new URL(String(u), location.href).protocol) ? String(u) : ''; } catch (e) { return ''; } };
  // the service registry and label tables (src/service), from the page
  const DATA = (() => { try { return JSON.parse(document.getElementById('w-data').textContent); } catch (e) { return {}; } })();
  const SERVICES = DATA.services || {}, LABELS = DATA.collection || {}, SCOPE = DATA.scope || {};
  const service = kind => SERVICES[kind] || {};

  // Lucide line icons (lucide.dev, ISC), the same set as templates/_icons.html
  const ICONS = {
    lock: '<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
    unlock: '<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 9.9-1"/>',
    shield: '<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="m9 12 2 2 4-4"/>',
    key: '<path d="M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z"/><circle cx="16.5" cy="7.5" r=".5" fill="currentColor"/>',
    error: '<circle cx="12" cy="12" r="10"/><path d="m15 9-6 6"/><path d="m9 9 6 6"/>',
    warning: '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
    info: '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
    ok: '<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>',
    external: '<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>',
    database: '<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5V19A9 3 0 0 0 21 19V5"/><path d="M3 12A9 3 0 0 0 21 12"/>',
  };
  const icon = (name, cls = '') => `<svg class="ico-i ${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICONS[name] || ''}</svg>`;

  let toastT;
  function toast(msg, err) { const t = document.getElementById('toast'); if (!t) return; t.textContent = msg; t.hidden = false;
    t.classList.toggle('err', !!err); clearTimeout(toastT); toastT = setTimeout(() => t.hidden = true, 5000); }

  // A modal instead of the browser's confirm()/prompt(): resolves true/false, or with `input` the text (null if cancelled).
  //   ask({ title, body, ok: 'Delete', danger: true })   body: text, paragraphs split on blank lines
  //   ask({ title, input: { value, placeholder, type, autocomplete } })
  function ask({ title, body = '', ok = 'OK', cancel = 'Cancel', danger = false, input = null }) {
    return new Promise(resolve => {
      const d = document.createElement('dialog');
      d.className = 'dlg modal';
      d.setAttribute('aria-labelledby', 'modal-title');
      const paras = String(body).split(/\n\s*\n/).filter(Boolean).map(p => `<p>${esc(p).replace(/\n/g, '<br>')}</p>`).join('');
      d.innerHTML = `<form method="dialog"><h3 id="modal-title">${esc(title)}</h3>${paras ? `<div class="modal-body">${paras}</div>` : ''}
        ${input ? `<input name="v" type="${esc(input.type || 'text')}" autocomplete="${esc(input.autocomplete || 'off')}"
          placeholder="${esc(input.placeholder || '')}" ${input.required ? 'required' : ''}>` : ''}
        <div class="modal-actions"><button class="btn ghost" type="button" data-cancel>${esc(cancel)}</button>
          <button class="btn ${danger ? 'danger-fill' : 'primary'}" value="ok">${esc(ok)}</button></div></form>`;
      const field = d.querySelector('input[name=v]');
      if (field) field.value = input.value || '';
      d.querySelector('[data-cancel]').addEventListener('click', () => d.close('cancel'));
      d.addEventListener('click', e => { if (e.target === d) d.close('cancel'); });     // a click on the backdrop
      d.addEventListener('close', () => {
        const yes = d.returnValue === 'ok';
        d.remove();
        const v = field && (field.type === 'password' ? field.value : field.value.trim());   // a password is taken as typed
        resolve(field ? (yes ? v : null) : yes);
      });
      document.body.append(d);
      d.showModal();
      // the safe choice has the focus on anything destructive; otherwise the field, or the button
      (field || d.querySelector(danger ? '[data-cancel]' : 'button[value=ok]')).focus();
      if (field) field.select();
    });
  }

  async function api(method, url, body) {
    const r = await fetch(url, { method, headers: body ? {'Content-Type': 'application/json'} : {}, body: body ? JSON.stringify(body) : undefined });
    let j = {}; try { j = await r.json(); } catch (e) {}
    if (r.status === 401 && !url.startsWith('/api/auth/')) {        // signed out (expired, or ended elsewhere)
      location.href = '/login?next=' + encodeURIComponent(location.pathname + location.search);
      throw new Error('signed out');
    }
    if (!r.ok) throw new Error(j.error || j.detail || r.statusText);
    return j;
  }

  function renderApp(el, a) {
    el.querySelector('.dot').className = 'dot ' + (a.online ? 'online' : 'offline');
    el.querySelector('.app-version').textContent = a.online ? `${a.version || ''}${a.database_type ? ' · ' + a.database_type : ''}` : 'offline';
    const col = el.querySelector('.app-collection');
    if (!a.online) { col.innerHTML = `<span class="error">${esc(a.error || 'unreachable')}</span>`; return; }
    const c = a.collection || {}; let h = '';
    for (const [k, v] of Object.entries(c)) if (k !== 'size_on_disk') h += `<span class="stat"><b>${fmtN(v)}</b> ${esc(LABELS[k] || k)}</span>`;
    if ('size_on_disk' in c) h += `<span class="stat"><b>${fmtBytes(c.size_on_disk)}</b> on disk</span>`;
    col.className = 'app-collection'; col.innerHTML = h || '<span class="muted">no collection data</span>';
    const s = a.scope || {}; let sh = '';
    for (const [k, v] of Object.entries(s)) if (k !== 'root_free_space') sh += `<span class="stat"><b>${esc(v)}</b> ${esc(SCOPE[k] || k)}</span>`;
    if (s.root_free_space) sh += `<span class="stat"><b>${fmtBytes(s.root_free_space)}</b> free</span>`;
    el.querySelector('.app-scope').innerHTML = sh;
    let dh = ''; for (const d of (a.databases || [])) dh += `<span class="stat">${icon('database')} <b>${esc(d.name)}</b> ${fmtBytes(d.size)}</span>`;
    el.querySelector('.app-db').innerHTML = dh;
    if (a.health && a.health.length) { const hd = document.createElement('div'); hd.className = 'health';
      hd.innerHTML = icon('warning') + ' ' + esc(a.health.map(x => x.message).join(' · ')); el.appendChild(hd); }
  }

  async function loadOverview(suite, refresh) {
    const box = document.getElementById('overview-' + suite); if (!box) return;
    try {
      const o = await api('GET', `/api/suites/${suite}/overview${refresh ? '?refresh=1' : ''}`);
      box.querySelectorAll('.health').forEach(x => x.remove());
      for (const a of o.apps) { const el = box.querySelector(`[data-app="${CSS.escape(a.name)}"]`); if (el) renderApp(el, a); }
    } catch (e) { toast('overview: ' + e.message, true); }
  }

  async function loadVpn() {
    const el = document.getElementById('vpn-pill'); if (!el) return;
    try {
      const v = await api('GET', '/api/vpn');
      if (!v.configured) { el.hidden = true; return; }
      el.hidden = false; el.className = 'vpn-pill ' + (v.ok ? 'ok' : 'bad');
      el.innerHTML = `<span class="dot"></span>VPN ${esc(v.ok ? 'up' : (v.status || 'down'))}${v.public_ip ? ' · ' + esc(v.public_ip) : ''}${v.forwarded_port ? ' · port ' + esc(v.forwarded_port) : ''}`;
      el.title = v.error || `gluetun ${v.name}`;
    } catch (e) { el.hidden = false; el.className = 'vpn-pill bad'; el.innerHTML = '<span class="dot"></span>VPN ?'; }
  }

  async function runAction(btn, method, url, body, label) {
    btn.disabled = true;
    try { const r = await api(method, url, body); toast(`${label} started (run #${r.run})`); location.href = `/runs/${r.run}`; }
    catch (e) { toast(`${label} failed: ${e.message}`, true); btn.disabled = false; }
  }

  function followRun(id, status) {
    if (status !== 'queued' && status !== 'running') return;
    const tick = async () => {
      try {
        const r = await api('GET', `/api/runs/${id}`);
        const log = document.getElementById('run-log'); log.textContent = r.log; log.scrollTop = log.scrollHeight;
        document.getElementById('run-status').innerHTML = `<span class="pill pill-${esc(r.status)}">${esc(r.status)}</span>`;
        if (r.status === 'queued' || r.status === 'running') setTimeout(tick, 2000); else location.reload();
      } catch (e) { setTimeout(tick, 5000); }
    };
    setTimeout(tick, 1500);
  }

  function theme(next) {
    const root = document.documentElement;
    if (!next) { try { next = localStorage.getItem('wranglarr-theme'); } catch (e) {} }
    if (next) root.dataset.theme = next;
  }
  function toggleTheme() {
    const dark = matchMedia('(prefers-color-scheme: dark)').matches;
    const cur = document.documentElement.dataset.theme || (dark ? 'dark' : 'light');
    const next = cur === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem('wranglarr-theme', next); } catch (e) {}
  }

  document.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    const d = b.dataset;
    if (d.backup) runAction(b, 'POST', `/api/suites/${d.backup}/backup`, null, 'backup');
    else if (d.sync) runAction(b, 'POST', `/api/suites/${d.sync}/sync`, {}, 'sync');
    else if (d.refresh) { b.disabled = true; loadOverview(d.refresh, true).finally(() => b.disabled = false); }
    else if ('eventsWire' in d) { b.disabled = true;
      api('POST', '/api/events/wire').then(r => { const bad = r.results.filter(x => x.error);
        toast(bad.length ? `${bad.map(x => x.app).join(', ')}: ${bad[0].error}` : 'webhooks in place', !!bad.length); setTimeout(() => location.reload(), 1200); })
        .catch(err => { toast(err.message, true); b.disabled = false; }); }
    else if (d.prune) api('POST', `/api/suites/${d.prune}/prune`).then(r => { toast(`pruned ${r.pruned.length} version(s)`); if (r.pruned.length) location.reload(); }).catch(err => toast(err.message, true));
    else if (d.reset) api('DELETE', `/api/suites/${d.reset}/settings`).then(() => location.reload()).catch(err => toast(err.message, true));
    else if (d.deleteVersion) ask({ title: `Delete version ${d.deleteVersion}?`, body: 'This cannot be undone.', ok: 'Delete', danger: true })
      .then(yes => yes && api('DELETE', `/api/suites/${d.suite}/versions/${d.deleteVersion}`).then(() => location.reload()).catch(err => toast(err.message, true)));
    else if (d.restore) { const body = JSON.parse(d.restore);
      const what = body.databases && body.config ? 'databases AND config' : body.databases ? 'databases' : 'config';
      ask({ title: `Restore ${body.app} ${what} from ${d.version}?`, body: "This overwrites the app's current state and restarts it.", ok: 'Restore', danger: true })
        .then(yes => yes && runAction(b, 'POST', `/api/suites/${d.suite}/versions/${d.version}/restore`, body, 'restore')); }
    else if (d.planRun !== undefined) ask({ title: 'Run the pending restore plan?', body: 'Each listed app is restored (databases + config) and restarted.', ok: 'Run it', danger: true })
      .then(yes => yes && runAction(b, 'POST', '/api/restore-plan/run', null, 'restore plan'));
    else if (d.planDiscard !== undefined) api('DELETE', '/api/restore-plan').then(() => location.reload());
    else if (d.updatesCheck !== undefined) { b.disabled = true; toast('asking the registries…');
      api('POST', '/api/updates/check').then(r => { toast(`${r.available} update(s) available`); location.reload(); })
        .catch(err => { toast(err.message, true); b.disabled = false; }); }
    else if (d.updatesApply !== undefined) ask({ title: 'Update every container with a newer image?', ok: 'Update all',
        body: 'Held ones are skipped. Affected suites are backed up first; each container is replaced and restarted.' })
      .then(yes => yes && runAction(b, 'POST', '/api/updates/apply', {}, 'update'));
    else if (d.updateOne) ask({ title: `Update ${d.updateOne}?`, ok: 'Update',
        body: 'Its suite is backed up first, then the container is replaced and restarted.' })
      .then(yes => yes && runAction(b, 'POST', '/api/updates/apply', {containers: [d.updateOne]}, 'update'));
    else if (d.selfUpdate !== undefined) ask({ title: 'Update wranglarr now?', ok: 'Update now',
        body: "wranglarr backs up its own state, then a helper replaces its container with the new image. Its pages go away for a minute and the run's page picks up again when it is back.\n\nIf the new one doesn't come up, the old one is put back." })
      .then(yes => yes && runAction(b, 'POST', '/api/updates/self', null, 'wranglarr update'));
    else if (d.rollback) ask({ title: `Roll ${d.rollback} back to ${d.to}?`, ok: 'Roll back', danger: true,
        body: 'The container is recreated on the previous image and held, so automatic updates leave it alone.\n\nIts data is not rolled back: restore a pre-update backup for that.' })
      .then(yes => yes && runAction(b, 'POST', '/api/updates/rollback', {container: d.rollback}, 'rollback'));
    else if (d.hold) api('POST', '/api/updates/hold', {container: d.hold, hold: d.on === '1'}).then(() => location.reload()).catch(err => toast(err.message, true));
    else if (d.updatesReset !== undefined) api('DELETE', '/api/updates/settings').then(() => location.reload()).catch(err => toast(err.message, true));
    else if (d.trackReset) api('DELETE', `/api/updates/tracks/${encodeURIComponent(d.trackReset)}`).then(() => location.reload()).catch(err => toast(err.message, true));
    else if (d.connect !== undefined) { const links = d.connect ? [d.connect] : [];
      ask(links.length ? { title: `Connect ${d.label}?`, ok: 'Connect' }
        : { title: 'Connect the apps?', ok: 'Connect',
            body: 'Every download client into every arr, Sonarr and Radarr into Bazarr, the arrs into Prowlarr.\n\nExisting entries with the same name or host are updated.' })
        .then(yes => yes && runAction(b, 'POST', '/api/connect', {mode: 'all', links}, 'connect')); }
    else if (d.ixNew !== undefined) ixForm(null);
    else if (d.ixEdit) ixForm(JSON.parse(document.getElementById('ix-sources').textContent)[d.ixEdit]);
    else if (d.ixCancel !== undefined) document.getElementById('ix-form-panel').hidden = true;
    else if (d.ixDelete) ask({ title: `Remove ${d.name}?`, body: d.type === 'prowlarr' ? 'wranglarr stops managing it. The arrs stay registered in that Prowlarr: remove them there if you want them gone.' : 'The indexers wranglarr added for it are removed from the arrs.', ok: 'Remove', danger: true })
      .then(yes => yes && api('DELETE', `/api/indexers/sources/${d.ixDelete}`).then(r => r.run ? location.href = `/runs/${r.run}` : location.reload()).catch(err => toast(err.message, true)));
    else if (d.ixSync !== undefined) runAction(b, 'POST', '/api/indexers/sync', null, 'indexer sync');
    else if (d.theme !== undefined) toggleTheme();
    else if (d.vpnRefresh !== undefined) { api('GET', '/api/vpn?refresh=1').then(() => location.reload()); }
  });
  const IX_HINT = {
    torznab: ["A torrent tracker's Torznab feed (from the tracker's site, or one Jackett/Prowlarr tracker's feed URL).", 'the feed\'s API key (blank if it has none)'],
    newznab: ["A usenet indexer's Newznab API, e.g. https://api.nzbgeek.info", "your account's API key"],
    jackett: ['Every tracker configured in Jackett is added to the arrs as its own Torznab indexer.', "Jackett's API key (top of its dashboard)"],
    nzbhydra: ['NZBHydra2 searches all its indexers; the arrs get one Newznab indexer (and Torznab if ticked).', "Hydra's API key (Config → Main)"],
    prowlarr: ['A Prowlarr outside this stack: the picked arrs are registered in it and it pushes its own indexers.', "Prowlarr's API key (Settings → General)"]};
  function ixShow(f) {
    const t = f.elements.type.value;
    f.querySelector('[data-ix-hint]').textContent = IX_HINT[t][0];
    f.querySelector('[data-ix-keyhint]').textContent = IX_HINT[t][1] + (f.elements.id.value ? ' · leave blank to keep the saved one' : '');
    f.querySelectorAll('[data-ix-only]').forEach(r => r.hidden = !r.dataset.ixOnly.split(' ').includes(t));
  }
  function ixForm(src) {
    const panel = document.getElementById('ix-form-panel'), f = panel.querySelector('form');
    f.reset(); f.elements.id.value = src ? src.id : '';
    document.getElementById('ix-form-title').textContent = src ? `Edit ${src.name}` : 'Add tracker or aggregator';
    if (src) {
      for (const k of ['type', 'name', 'url', 'priority', 'min_seeders']) f.elements[k].value = src[k];
      f.elements.enabled.checked = src.enabled; f.elements.torrents.checked = !!src.torrents;
      f.querySelectorAll('[name=apply_to]').forEach(c => c.checked = !src.apply_to.length || src.apply_to.includes(c.value));
    }
    ixShow(f); panel.hidden = false; panel.scrollIntoView({behavior: 'smooth'});
  }
  document.addEventListener('change', e => { const f = e.target.closest('form[data-ix-form]'); if (f && e.target.name === 'type') ixShow(f); });
  document.addEventListener('submit', e => {
    const track = e.target.closest('form[data-update-track]');
    if (track) {
      e.preventDefault();
      const btn = track.querySelector('[type=submit]'); btn.disabled = true;
      api('PUT', `/api/updates/tracks/${encodeURIComponent(track.dataset.updateTrack)}`, {tag: track.elements.tag.value.trim() || null})
        .then(() => location.reload()).catch(err => { toast(err.message, true); btn.disabled = false; });
      return;
    }
    const fx = e.target.closest('form[data-ix-form]');
    if (fx) {
      e.preventDefault();
      const el = fx.elements, picked = [...fx.querySelectorAll('[name=apply_to]')], on = picked.filter(c => c.checked).map(c => c.value);
      if (!on.length) { toast('pick at least one arr', true); return; }
      api('POST', '/api/indexers/sources', { id: el.id.value, type: el.type.value, name: el.name.value, url: el.url.value,
        api_key: el.api_key.value, apply_to: on.length === picked.length ? [] : on, enabled: el.enabled.checked,
        priority: +el.priority.value, min_seeders: +el.min_seeders.value, torrents: el.torrents.checked })
        .then(r => { toast('saved'); location.href = r.run ? `/runs/${r.run}` : '/indexers'; }).catch(err => toast(err.message, true));
      return;
    }
    const f = e.target.closest('form[data-update-settings]'); if (!f) return;
    e.preventDefault();
    const v = n => f.elements[n];
    api('PUT', '/api/updates/settings', { enabled: v('enabled').checked, check_schedule: v('check_schedule').value,
      auto_update: v('auto_update').checked, auto_schedule: v('auto_schedule').value, backup_first: v('backup_first').checked,
      self_update: v('self_update').checked, self_schedule: v('self_schedule').value })
      .then(() => { toast('saved'); location.reload(); }).catch(err => toast(err.message, true));
  });
  theme();
  document.addEventListener('DOMContentLoaded', loadVpn);

  // Live status: the arrs tell wranglarr when something changes, and it tells the open pages.
  // Each message arrives as a `wranglarr:event` on document (detail: the message); a page with
  // nothing that changes doesn't open the stream.
  function listen() {
    const wants = document.querySelector('[id^="overview-"], .app-col[data-suite], [data-live]');
    if (!wants || !window.EventSource) return;
    let seen = false;
    const es = new EventSource('/api/events/stream');
    es.onmessage = m => {
      let d; try { d = JSON.parse(m.data); } catch (e) { return; }
      // the first hello is this page opening; a later one, a reconnect that may have missed something
      if (d.type === 'hello' && !seen) { seen = true; return; }
      if (d.type === 'hello') d = { type: 'resync' };
      if (d.type === 'overview') loadOverview(d.suite, false);
      else if (d.type === 'resync') document.querySelectorAll('[id^="overview-"]').forEach(b => loadOverview(b.id.slice(9), false));
      document.dispatchEvent(new CustomEvent('wranglarr:event', { detail: d }));
    };
    addEventListener('pagehide', () => es.close());
  }
  document.addEventListener('DOMContentLoaded', listen);

  // System → Dashboard: what the dashboard shows
  const hf = document.getElementById('home-form');
  if (hf) {
    hf.addEventListener('submit', async e => {
      e.preventDefault();
      const el = hf.elements, body = { details: el.details.value, days: el.days.value, release_next: el.release_next.value, release_days: el.release_days.value,
                                         missing_days: el.missing_days.value, limit: el.limit.value };
      for (const k of ['upcoming', 'unmonitored', 'missing', 'queue', 'requests', 'indexers']) body[k] = el[k].checked;
      try { await api('PUT', '/api/home/settings', body); toast('saved'); } catch (err) { toast(err.message, true); }
    });
    hf.querySelector('[data-home-reset]').addEventListener('click', async () => {
      try { await api('DELETE', '/api/home/settings'); location.reload(); } catch (err) { toast(err.message, true); }
    });
  }

  // phones: the sidebar is a drawer behind the header's menu button
  const setNav = open => {
    document.body.classList.toggle('nav-open', open);
    document.querySelectorAll('.nav-scrim').forEach(x => x.hidden = !open);
    document.querySelectorAll('.nav-toggle').forEach(x => x.setAttribute('aria-expanded', String(open)));
  };
  document.addEventListener('click', e => {
    if (e.target.closest('[data-nav-toggle]')) setNav(!document.body.classList.contains('nav-open'));
    else if (e.target.closest('.sidebar a')) setNav(false);
  });
  document.addEventListener('keydown', e => { if (e.key === 'Escape' && document.body.classList.contains('nav-open')) setNav(false); });
  matchMedia('(min-width: 901px)').addEventListener('change', e => { if (e.matches) setNav(false); });

  // a small logo an app doesn't serve: show its emoji instead
  // a logo that doesn't load: the kind's shipped one (data-fallback), then nothing
  const logoFallback = img => { const f = img.dataset.fallback; img.removeAttribute('data-fallback');
    if (f) img.src = f; else img.style.visibility = 'hidden'; };
  document.querySelectorAll('img[data-fallback]').forEach(img => { if (img.complete && !img.naturalWidth) logoFallback(img); });
  document.addEventListener('error', e => { if (e.target.matches?.('img[data-fallback]')) logoFallback(e.target);
    else if (e.target.matches?.('img.ico-logo, img.mini-logo, img.dl-arr')) e.target.style.visibility = 'hidden'; }, true);

  return { loadOverview, followRun, toast, api, esc, safeUrl, fmtBytes, fmtN, ask, icon, service, LABELS };
})();
