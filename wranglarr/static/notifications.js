// Notifications: channels, what each one gets, the watcher's settings, and what was sent.
(() => {
  const { api, esc, toast } = W;
  let D = null, editing = null;              // overview from the server; the channel being edited
  const $ = id => document.getElementById(id);
  const editor = $('editor'), form = $('ch-form');
  const when = s => s ? new Date(s).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' }) : '';
  const LEVEL = Object.fromEntries(['error', 'warning', 'info', 'ok'].map(l => [l, W.icon(l, 'lv-' + l)]));

  async function load() {
    try { D = await api('GET', '/api/notify'); } catch (e) { toast(e.message, true); return; }
    renderChannels(); renderWatch(); renderLog();
  }

  function renderChannels() {
    const box = $('channels');
    if (!D.channels.length) {
      box.innerHTML = `<p class="muted">No channels yet. Pick where wranglarr should send messages:</p>
        <div class="kind-grid">${kindButtons()}</div>`;
      return;
    }
    box.innerHTML = `<table class="compact channels"><tbody>${D.channels.map(c => {
      const k = D.kinds[c.kind] || {};
      const ev = c.events.length === Object.keys(D.events).length ? 'everything' : `${c.events.length} of ${Object.keys(D.events).length} events`;
      const arrs = c.arr.apps.length ? ` · in ${c.arr.apps.map(esc).join(', ')}` : '';
      return `<tr data-id="${esc(c.id)}" class="${c.enabled ? '' : 'off'}">
        <td class="k-ico">${kIco(k)}</td>
        <td><b>${esc(c.name)}</b> <span class="muted small">${esc(k.label || c.kind)}${c.enabled ? '' : ' · off'}</span>
          <div class="muted small">${ev}${arrs}</div></td>
        <td class="num"><button class="btn small" data-act="test">Test</button>
          <button class="btn small" data-act="edit">Edit</button>
          <button class="btn small ghost danger" data-act="delete">Delete</button></td></tr>`;
    }).join('')}</tbody></table>`;
  }

  // the service's logo (shipped in /static/icons), or its emoji
  const kIco = k => k.logo ? `<img class="ico-logo" src="${esc(k.logo)}" alt="">` : esc(k.icon || '•');

  const kindButtons = () => Object.entries(D.kinds).map(([k, v]) =>
    `<button class="kind-btn" type="button" data-kind="${esc(k)}"><span class="k-ico">${kIco(v)}</span>${esc(v.label)}</button>`).join('');

  function openEditor(c) {
    editing = c || null;
    editor.hidden = false;
    $('ch-result').textContent = '';
    if (!c) { $('ed-title').textContent = 'Add a channel'; $('kind-grid').innerHTML = kindButtons(); $('kind-grid').hidden = false; form.hidden = true; }
    else fillForm(c.kind, c);
    editor.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }

  function fillForm(kind, c) {
    const k = D.kinds[kind];
    $('kind-grid').hidden = true; form.hidden = false; form.dataset.kind = kind;
    $('ed-title').textContent = c ? `${k.label}: ${c.name}` : `Add ${k.label}`;
    form.elements.name.value = c ? c.name : (D.channels.some(x => x.name === k.label) ? `${k.label} 2` : k.label);
    form.elements.enabled.checked = c ? c.enabled : true;
    const cfg = c ? c.config : {};
    $('ch-fields').innerHTML = k.fields.map(f => {
      const v = cfg[f.key] ?? f.default ?? '';
      let input;
      if (f.type === 'select') input = `<select name="f:${esc(f.key)}" style="width:auto">${f.options.map(([o, l]) => `<option value="${esc(o)}" ${o === v ? 'selected' : ''}>${esc(l)}</option>`).join('')}</select>`;
      else input = `<input name="f:${esc(f.key)}" type="${f.type === 'secret' ? 'password' : f.type === 'number' ? 'number' : 'text'}" value="${esc(v)}"
        placeholder="${esc(f.placeholder)}" spellcheck="false" ${f.type === 'secret' ? 'autocomplete="new-password"' : ''}>`;
      return `<div class="form-row"><label class="l">${esc(f.label)}${f.required ? '' : ' <span class="muted small">(optional)</span>'}</label>
        <div>${input}${f.hint ? `<div class="hint">${esc(f.hint)}</div>` : ''}${f.type === 'secret' && v === D.mask ? '<div class="hint">saved; leave as is to keep it</div>' : ''}</div></div>`;
    }).join('');
    const evs = c ? new Set(c.events) : new Set(Object.entries(D.events).filter(([, e]) => e.default).map(([id]) => id));
    $('ch-events').innerHTML = Object.entries(D.events).map(([id, e]) =>
      `<label class="check-line"><input type="checkbox" name="ev" value="${esc(id)}" ${evs.has(id) ? 'checked' : ''}> ${LEVEL[e.level] || ''} ${esc(e.label)}</label>`).join('');
    const arrApps = new Set(c ? c.arr.apps : []), arrEv = new Set(c ? c.arr.events : D.arr_events_default);
    $('ch-arrs').innerHTML = D.arrs.length ? D.arrs.map(a =>
      `<label class="check-line"><input type="checkbox" name="arr" value="${esc(a.name)}" ${arrApps.has(a.name) ? 'checked' : ''}>
        <img class="mini-logo" src="/api/apps/${encodeURIComponent(a.name)}/logo" alt="" onerror="this.remove()"> ${esc(a.name)}</label>`).join('')
      : '<span class="muted small">no arrs in this stack</span>';
    $('ch-arr-events').innerHTML = Object.entries(D.arr_events).map(([id, l]) =>
      `<label class="check-line"><input type="checkbox" name="arrev" value="${esc(id)}" ${arrEv.has(id) ? 'checked' : ''}> ${esc(l)}</label>`).join('');
    showArrEvents();
  }
  const showArrEvents = () => { $('ch-arr-events').hidden = !form.querySelector('input[name=arr]:checked'); };

  function body() {
    const el = form.elements, config = {};
    form.querySelectorAll('[name^="f:"]').forEach(i => config[i.name.slice(2)] = i.value);
    const all = n => [...form.querySelectorAll(`input[name=${n}]:checked`)].map(i => i.value);
    return { kind: form.dataset.kind, name: el.name.value, enabled: el.enabled.checked, config,
      events: all('ev'), arr: { apps: all('arr'), events: all('arrev') } };
  }

  const arrResult = r => (r.arrs || []).filter(x => x.action !== 'none').map(x => x.error ? `${x.app}: ${x.error}` : `${x.app}: ${x.action}`).join(' · ');

  form.addEventListener('submit', async e => {
    e.preventDefault();
    const btn = form.querySelector('[type=submit]'); btn.disabled = true;
    try {
      const r = editing ? await api('PUT', `/api/notify/channels/${editing.id}`, body()) : await api('POST', '/api/notify/channels', body());
      const bad = (r.arrs || []).filter(x => x.error);
      toast(`saved${arrResult(r) ? ' · ' + arrResult(r) : ''}`, bad.length > 0);
      if (!bad.length) { editor.hidden = true; editing = null; } else { editing = r.channel; $('ch-result').innerHTML = `<span class="error">${esc(arrResult(r))}</span>`; }
      await load();
    } catch (err) { $('ch-result').innerHTML = `<span class="error">${esc(err.message)}</span>`; }
    finally { btn.disabled = false; }
  });
  form.addEventListener('change', e => { if (e.target.name === 'arr') showArrEvents(); });

  async function test(b, ch, id) {
    b.disabled = true; const t = b.textContent; b.textContent = 'sending…';
    try {
      const r = await api('POST', '/api/notify/test', { channel: ch, id });
      toast(r.ok ? 'sent: check it arrived' : r.error, !r.ok);
      if (b.dataset.test !== undefined) $('ch-result').innerHTML = r.ok ? '<span class="ok-text">sent</span>' : `<span class="error">${esc(r.error)}</span>`;
    } catch (err) { toast(err.message, true); }
    finally { b.disabled = false; b.textContent = t; }
  }

  document.addEventListener('click', async e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.matches('[data-add]')) openEditor(null);
    else if (b.dataset.kind) { if (editor.hidden) openEditor(null); editing = null; fillForm(b.dataset.kind, null); }
    else if (b.matches('[data-cancel]')) { editor.hidden = true; editing = null; }
    else if (b.matches('[data-test]')) test(b, body(), editing && editing.id);
    else if (b.dataset.ev) {
      form.querySelectorAll('input[name=ev]').forEach(i => i.checked = b.dataset.ev === 'all' || (b.dataset.ev === 'default' && D.events[i.value].default));
    } else if (b.dataset.act) {
      const c = D.channels.find(x => x.id === b.closest('tr').dataset.id); if (!c) return;
      if (b.dataset.act === 'edit') openEditor(c);
      else if (b.dataset.act === 'test') test(b, c, c.id);
      else if (b.dataset.act === 'delete') {
        if (!await W.ask({ title: `Delete ${c.name}?`, ok: 'Delete', danger: true,
            body: c.arr.apps.length ? `It's removed from ${c.arr.apps.join(', ')} too.` : '' })) return;
        try { const r = await api('DELETE', `/api/notify/channels/${c.id}`); toast('deleted' + (arrResult(r) ? ' · ' + arrResult(r) : '')); load(); }
        catch (err) { toast(err.message, true); }
      }
    } else if (b.matches('[data-check]')) {
      b.disabled = true;
      try { const r = await api('POST', '/api/notify/check');
        toast(r.skipped || `${r.problems} open problem(s), ${r.sent} new message(s)`); load(); }
      catch (err) { toast(err.message, true); } finally { b.disabled = false; }
    }
  });

  // ---- the watcher ----
  const wform = $('watch-form');
  function renderWatch() {
    for (const k of ['interval', 'disk_free_gb', 'stall_minutes', 'link']) if (document.activeElement !== wform.elements[k]) wform.elements[k].value = D.settings[k];
    const on = D.channels.some(c => c.enabled);
    $('watching').innerHTML = !on ? '<span class="muted">Nothing is watched until a channel is on.</span>'
      : D.watching.length ? `<ul class="plain">${D.watching.map(w => `<li>${LEVEL[(D.events[w.event] || {}).level] || ''} <b>${esc(w.title)}</b> <span class="muted">${esc(w.message)}</span>
          <span class="muted">since ${esc(when(w.since))}${w.sent ? ', sent' : ', waiting to confirm'}</span></li>`).join('')}</ul>`
      : '<span class="muted">Nothing wrong at the last check.</span>';
  }
  wform.addEventListener('submit', async e => {
    e.preventDefault();
    const el = wform.elements;
    try { await api('PUT', '/api/notify/settings', { interval: el.interval.value, disk_free_gb: el.disk_free_gb.value,
      stall_minutes: el.stall_minutes.value, link: el.link.value }); toast('saved'); load(); }
    catch (err) { toast(err.message, true); }
  });

  function renderLog() {
    $('log').innerHTML = D.log.length ? D.log.map(l => `<tr><td class="small nowrap">${esc(when(l.at))}</td>
      <td>${LEVEL[l.level] || ''} <b>${esc(l.title)}</b><div class="muted small msg">${esc(l.message)}</div></td>
      <td class="small">${l.sent.length ? l.sent.map(s => s.ok ? esc(s.channel) : `<span class="error" title="${esc(s.error)}">${esc(s.channel)} ✗</span>`).join(', ')
        : '<span class="muted">no channel wanted it</span>'}</td></tr>`).join('')
      : '<tr><td class="muted" colspan="3">Nothing sent yet.</td></tr>';
  }

  load();
})();
