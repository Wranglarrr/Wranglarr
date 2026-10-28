// Connections: media servers pushed into the arrs, and Seerr.
(() => {
  const { api, esc, safeUrl, toast } = W;
  const $ = id => document.getElementById(id);
  let D = null, editing = null;
  const editor = $('editor'), form = $('ms-form');

  async function load() {
    try { D = await api('GET', '/api/connections'); } catch (e) { toast(e.message, true); return; }
    renderServers(); renderSeerr();
  }

  const kindButtons = () => Object.entries(D.media.kinds).map(([k, v]) =>
    `<button class="kind-btn" type="button" data-kind="${esc(k)}"><span class="k-ico"><img class="ico-logo" src="${esc(v.icon)}" alt=""></span>${esc(v.label)}</button>`).join('');

  function renderServers() {
    const box = $('servers'), M = D.media;
    if (!M.servers.length) { box.innerHTML = `<p class="muted">None yet. Add yours:</p><div class="kind-grid">${kindButtons()}</div>`; return; }
    box.innerHTML = `<table class="compact channels"><tbody>${M.servers.map(m => {
      const k = M.kinds[m.kind];
      return `<tr data-id="${esc(m.id)}"><td class="k-ico"><img class="ico-logo" src="${esc(k.icon)}" alt=""></td>
        <td><b>${esc(m.name)}</b> <span class="muted small">${esc(k.label)} · ${esc(m.url)}</span>
          <div class="muted small">${m.arrs.length ? 'in ' + m.arrs.map(esc).join(', ') : 'in no arr yet'}${m.map_from ? ` · ${esc(m.map_from)} → ${esc(m.map_to)}` : ''}${m.update_library ? '' : ' · no library rescan'}</div></td>
        <td class="num"><button class="btn small" data-act="test">Test</button> <button class="btn small" data-act="edit">Edit</button>
          <button class="btn small ghost danger" data-act="delete">Delete</button></td></tr>`;
    }).join('')}</tbody></table>`;
  }

  function renderSeerr() {
    const box = $('seerr'), s = D.seerr;
    if (!s) {
      box.innerHTML = `<p>Seerr lets the people you share with ask for movies and shows; it hands each request to Radarr or Sonarr,
        and shows what's already in Plex, Jellyfin or Emby. Not in this stack yet. To add it, in the stack's folder:</p>
        <ol class="small"><li>add <code>seerr</code> to <code>COMPOSE_PROFILES</code> in <code>.env</code> (or tick it in the setup wizard);</li>
        <li>run <code>./scripts/init.sh</code>: it makes its database, adds it to wranglarr's backups and starts it;</li>
        <li>open it (port 5055) and sign in with your media server. wranglarr then adds Radarr and Sonarr to it.</li></ol>`;
      return;
    }
    const open = safeUrl(s.url) ? `<a class="btn small" href="${esc(safeUrl(s.url))}" target="_blank" rel="noopener">Open Seerr ↗</a>` : '';
    let state;
    if (!s.online) state = `<span class="error">offline${s.error ? ': ' + esc(s.error) : ''}</span>`;
    else if (!s.initialized) state = `<span class="warn-text">waiting for its setup</span>: open Seerr and sign in with Plex, Jellyfin or Emby.
      wranglarr checks every few minutes and adds Radarr and Sonarr once that's done.`;
    else state = `<span class="ok-text">ready</span>${s.version ? ' · ' + esc(s.version) : ''}`;
    const rows = s.links.map(l => `<tr><td>${esc(l.dst)}</td><td><span class="pill ${l.state === 'connected' ? 'pill-ok' : l.state === 'missing' ? '' : 'pill-partial'}">${esc(l.state === 'missing' ? 'not added' : l.state)}</span>
      <span class="muted small">${esc(l.detail || '')}</span></td></tr>`).join('');
    const can = s.initialized && s.links.some(l => l.state !== 'connected');
    box.innerHTML = `<p>${state} ${open}</p>
      ${rows ? `<table class="compact" style="width:auto"><tbody>${rows}</tbody></table>` : '<p class="muted small">No Radarr or Sonarr in the stack to add.</p>'}
      <div class="row" style="margin-top:8px"><button class="btn ${can ? 'primary' : ''}" data-seerr ${s.initialized ? '' : 'disabled'}>Add Radarr and Sonarr to Seerr</button>
        <span class="muted small">Picks each one's first quality profile and root folder; change them in Seerr → Settings → Services. Running it again keeps your changes.</span></div>`;
  }

  // ---- the editor ----
  function openEditor(m) {
    editing = m || null; editor.hidden = false; $('ms-result').textContent = '';
    if (!m) { $('ed-title').textContent = 'Add a media server'; $('kind-grid').innerHTML = kindButtons(); $('kind-grid').hidden = false; form.hidden = true; }
    else fill(m.kind, m);
    editor.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }
  function fill(kind, m) {
    const k = D.media.kinds[kind], el = form.elements;
    $('kind-grid').hidden = true; form.hidden = false; form.dataset.kind = kind;
    $('ed-title').textContent = m ? `${k.label}: ${m.name}` : `Add ${k.label}`;
    form.querySelectorAll('[data-only]').forEach(x => x.hidden = !x.dataset.only.split(' ').includes(kind));
    $('tok-label').textContent = k.secret_label;
    el.name.value = m ? m.name : k.label;
    el.url.value = m ? m.url : '';
    el.url.placeholder = `http://192.168.1.10:${k.port}`;
    el.token.value = m ? m.token || '' : ''; el.username.value = m ? m.username || '' : ''; el.password.value = m ? m.password || '' : '';
    el.update_library.checked = m ? m.update_library : true;
    el.map_from.value = m ? m.map_from : ''; el.map_to.value = m ? m.map_to : '';
    const on = new Set(m ? m.arrs : D.media.arrs.map(a => a.name));
    $('ms-arrs').innerHTML = D.media.arrs.map(a => `<label class="check-line"><input type="checkbox" name="arr" value="${esc(a.name)}" ${on.has(a.name) ? 'checked' : ''}>
      <img class="mini-logo" src="/api/apps/${encodeURIComponent(a.name)}/logo" alt="" onerror="this.remove()"> ${esc(a.name)}</label>`).join('') || '<span class="muted small">no arrs</span>';
  }
  function body() {
    const el = form.elements;
    return { kind: form.dataset.kind, name: el.name.value, url: el.url.value, token: el.token.value, username: el.username.value,
      password: el.password.value, update_library: el.update_library.checked, map_from: el.map_from.value, map_to: el.map_to.value,
      arrs: [...form.querySelectorAll('input[name=arr]:checked')].map(i => i.value) };
  }
  const arrResult = r => (r.arrs || []).filter(x => x.action !== 'none').map(x => x.error ? `${x.app}: ${x.error}` : `${x.app}: ${x.action}`).join(' · ');
  const result = (html) => { $('ms-result').innerHTML = html; };

  form.addEventListener('submit', async e => {
    e.preventDefault();
    const b = form.querySelector('[type=submit]'); b.disabled = true;
    try {
      const r = editing ? await api('PUT', `/api/connections/media/${editing.id}`, body()) : await api('POST', '/api/connections/media', body());
      const bad = (r.arrs || []).filter(x => x.error);
      toast('saved' + (arrResult(r) ? ' · ' + arrResult(r) : ''), bad.length > 0);
      if (bad.length) { editing = r.server; result(`<span class="error">${esc(arrResult(r))}</span>`); } else { editor.hidden = true; editing = null; }
      load();
    } catch (err) { result(`<span class="error">${esc(err.message)}</span>`); }
    finally { b.disabled = false; }
  });

  async function test(btn, srv, id) {
    btn.disabled = true;
    try {
      const r = await api('POST', '/api/connections/media/test', { server: srv, id });
      const msg = r.ok ? `works${r.version ? ' · ' + r.version : ''}${r.libraries && r.libraries.length ? ' · libraries: ' + r.libraries.join(', ') : ''}` : r.error;
      toast(msg, !r.ok);
      if (!form.hidden && btn.closest('form')) result(r.ok ? `<span class="ok-text">${esc(msg)}</span>` : `<span class="error">${esc(msg)}</span>`);
    } catch (err) { toast(err.message, true); } finally { btn.disabled = false; }
  }

  async function plexSignIn(btn) {
    const w = window.open('about:blank', 'plexauth', 'width=600,height=700');
    btn.disabled = true;
    try {
      const pin = await api('POST', '/api/connections/plex/pin'), auth = safeUrl(pin.auth_url);
      if (!auth) throw new Error('plex.tv sent no usable sign-in link');     // never a javascript: URL in our own popup
      if (w) w.location = auth; else { result(`<a href="${esc(auth)}" target="_blank" rel="noopener">Open plex.tv to allow wranglarr</a>`); }
      result('waiting for plex.tv…');
      for (let i = 0; i < 100; i++) {
        await new Promise(r => setTimeout(r, 3000));
        const r = await api('GET', `/api/connections/plex/pin/${pin.id}`);
        if (!r.done) continue;
        if (w && !w.closed) w.close();
        const s = r.servers[0];
        form.elements.token.value = s ? s.token : r.token;
        if (s && !form.elements.url.value && s.addresses.length) form.elements.url.value = s.addresses[0];
        result(`<span class="ok-text">signed in${s ? ': ' + esc(s.name) : ''}</span>` + (r.servers.length > 1 ? ` <span class="muted">(${esc(r.servers.length)} servers; this is the first)</span>` : '')
          + (s && s.addresses.length > 1 ? `<div class="muted small">its addresses: ${s.addresses.map(esc).join(', ')}</div>` : ''));
        return;
      }
      result('<span class="error">plex.tv sign-in timed out</span>');
    } catch (err) { result(`<span class="error">${esc(err.message)}</span>`); if (w) w.close(); }
    finally { btn.disabled = false; }
  }

  document.addEventListener('click', async e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.matches('[data-add]')) openEditor(null);
    else if (b.dataset.kind) { if (editor.hidden) openEditor(null); editing = null; fill(b.dataset.kind, null); }
    else if (b.matches('[data-cancel]')) { editor.hidden = true; editing = null; }
    else if (b.matches('[data-test]')) test(b, body(), editing && editing.id);
    else if (b.matches('[data-plex]')) plexSignIn(b);
    else if (b.matches('[data-jfkey]')) {
      const el = form.elements; b.disabled = true;
      try { const r = await api('POST', '/api/connections/jellyfin/key', { url: el.url.value, username: el.admin_user.value, password: el.admin_pass.value });
        el.token.value = r.token; el.admin_pass.value = ''; result('<span class="ok-text">API key created: save to keep it</span>'); }
      catch (err) { result(`<span class="error">${esc(err.message)}</span>`); } finally { b.disabled = false; }
    } else if (b.dataset.act) {
      const m = D.media.servers.find(x => x.id === b.closest('tr').dataset.id); if (!m) return;
      if (b.dataset.act === 'edit') openEditor(m);
      else if (b.dataset.act === 'test') test(b, m, m.id);
      else if (b.dataset.act === 'delete' && await W.ask({ title: `Delete ${m.name}?`, ok: 'Delete', danger: true,
          body: m.arrs.length ? `It's removed from ${m.arrs.join(', ')} too.` : '' })) {
        try { const r = await api('DELETE', `/api/connections/media/${m.id}`); toast('deleted' + (arrResult(r) ? ' · ' + arrResult(r) : '')); load(); }
        catch (err) { toast(err.message, true); }
      }
    } else if (b.matches('[data-seerr]')) {
      b.disabled = true;
      try { await api('POST', '/api/connect', { mode: 'all', links: D.seerr.links.map(l => l.key) });
        toast('adding them: see Activity'); setTimeout(load, 6000); }
      catch (err) { toast(err.message, true); } finally { setTimeout(() => b.disabled = false, 6000); }
    }
  });

  load();
})();
