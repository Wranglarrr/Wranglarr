// Reverse proxy: config scaffolds for this stack (wranglarr only shows text).
(() => {
  const { api, esc, toast } = W;
  const form = document.getElementById('proxy-form');
  const LABEL = { public: 'Public', lan: 'LAN only', off: 'Not proxied' };
  let S = null, R = null, timer = null;

  function fill() {
    for (const [k, v] of Object.entries(S)) {
      const el = form.elements[k]; if (!el || k === 'apps') continue;
      if (el instanceof RadioNodeList) el.value = v; else el.value = v;
    }
    form.querySelectorAll('[data-only]').forEach(r => r.hidden = !r.dataset.only.split(' ').includes(S.kind));
  }
  function apps() {
    document.getElementById('proxy-apps').innerHTML = R.plan.map(p => {
      const radios = Object.entries(LABEL).map(([v, l]) =>
        `<label class="inline"><input type="radio" name="ex:${esc(p.name)}" value="${v}" ${p.expose === v ? 'checked' : ''}> ${l}</label>`).join(' ');
      const at = p.expose === 'off' ? '<span class="muted">—</span>' : `https://${esc(p.fqdn)}${esc(p.path)}`;
      const up = p.up ? `<code>${esc(p.up)}</code>` : '<span class="warn-text">nowhere</span>';
      return `<tr><td>${esc(p.name)}</td><td class="small">${radios}</td>
        <td><input data-sub="${esc(p.name)}" value="${esc((S.apps[p.name] || {}).sub || '')}" placeholder="${esc(p.name)}" style="width:120px" spellcheck="false"></td>
        <td class="small">${at}</td><td class="small">${up}${p.note ? `<div class="warn-text">${esc(p.note)}</div>` : ''}</td></tr>`;
    }).join('');
  }
  function out() {
    document.getElementById('out-title').textContent = R.filename;
    document.getElementById('proxy-notes').innerHTML = R.notes.map(n => `<div class="check ${esc(n.cls)}">${esc(n.msg)}</div>`).join('');
    document.getElementById('proxy-how').textContent = R.how;
    document.getElementById('proxy-text').textContent = R.text;
  }
  function show(r) { S = r.settings; R = r; fill(); apps(); out(); }

  function collect() {
    const el = form.elements, body = { ...S, apps: { ...S.apps } };
    for (const k of ['kind', 'domain', 'style', 'upstream', 'upstream_host', 'lan', 'entrypoint', 'resolver', 'network']) body[k] = el[k].value;
    for (const p of R.plan) {
      const ex = document.querySelector(`[name="ex:${CSS.escape(p.name)}"]:checked`);
      const sub = document.querySelector(`[data-sub="${CSS.escape(p.name)}"]`);
      body.apps[p.name] = { expose: ex ? ex.value : p.expose, sub: sub ? sub.value.trim() : '' };
    }
    return body;
  }
  async function save(body) {
    try { show(await api('PUT', '/api/proxy', body || collect())); } catch (e) { toast(e.message, true); }
  }
  const later = () => { clearTimeout(timer); timer = setTimeout(() => save(), 400); };
  document.addEventListener('input', e => { if (e.target.closest('#proxy-form, #proxy-apps')) later(); });
  document.addEventListener('change', e => {
    if (e.target.closest('#proxy-form, #proxy-apps') && (e.target.type === 'radio' || e.target.tagName === 'SELECT')) { clearTimeout(timer); save(); }
  });
  document.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.dataset.preset) {
      const body = collect();
      for (const p of R.plan) {
        const arr = W.service(p.kind).public_preset || p.kind === 'wranglarr';
        body.apps[p.name].expose = { all: 'public', lan: 'lan', off: 'off', arrs: arr ? 'public' : 'lan' }[b.dataset.preset];
      }
      save(body);
    } else if (b.matches('[data-copy]')) {
      navigator.clipboard.writeText(R.text).then(() => toast('copied'), () => toast('copy failed: select the text and copy it', true));
    } else if (b.matches('[data-download]')) {
      const a = document.createElement('a');
      a.href = URL.createObjectURL(new Blob([R.text], { type: 'text/plain' })); a.download = R.filename; a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 1000);
    }
  });
  api('GET', '/api/proxy').then(show).catch(e => toast(e.message, true));
})();
