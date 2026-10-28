// Reverse proxy: config scaffolds for this stack (wranglarr only shows text).
(() => {
  const { api, esc, toast } = W;
  const form = document.getElementById('proxy-form');
  const WHO = { public: 'anyone', lan: 'local network only', off: '' };
  let S = null, R = null, timer = null;

  function showKind() {
    form.querySelectorAll('[data-only]').forEach(r => r.hidden = !r.dataset.only.split(' ').includes(form.elements.kind.value));
  }

  function fill() {
    for (const [k, v] of Object.entries(S)) {
      const el = form.elements[k]; if (!el || k === 'apps') continue;
      if (el instanceof RadioNodeList) el.value = v; else el.value = v;
    }
    showKind();
  }
  function apps() {
    document.getElementById('proxy-apps').innerHTML = R.plan.map(p => {
      const at = p.expose === 'off' ? '<span class="muted">direct IP, not proxied</span>' : `https://${esc(p.fqdn)}${esc(p.path)}`;
      const up = p.expose === 'off' ? '' : p.up ? `<code>${esc(p.up)}</code>` : '<span class="warn-text">nowhere</span>';
      return `<tr><td>${esc(p.name)}</td><td class="small">${at}</td><td class="small">${WHO[p.expose]}</td>
        <td class="small">${up}${p.note ? `<div class="warn-text">${esc(p.note)}</div>` : ''}</td></tr>`;
    }).join('');
  }
  function out() {
    document.getElementById('out-title').textContent = R.filename;
    document.getElementById('proxy-notes').innerHTML = R.notes.map(n => `<div class="check ${esc(n.cls)}">${esc(n.msg)}</div>`).join('');
    document.getElementById('proxy-how').textContent = R.how;
    document.getElementById('proxy-text').textContent = R.text;
  }
  function show(r) {
    S = r.settings; R = r; fill(); apps(); out();
    document.querySelectorAll('[data-copy], [data-download]').forEach(b => b.disabled = r.valid === false);
  }

  function collect() {
    const el = form.elements, body = {};
    for (const k of ['kind', 'upstream', 'upstream_host', 'entrypoint', 'resolver', 'network']) body[k] = el[k].value;
    return body;
  }
  async function save(body) {
    try { show(await api('PUT', '/api/proxy', body || collect())); } catch (e) { toast(e.message, true); }
  }
  const later = () => { clearTimeout(timer); timer = setTimeout(() => save(), 400); };
  document.addEventListener('input', e => { if (e.target.closest('#proxy-form')) later(); });
  document.addEventListener('change', e => {
    if (e.target.closest('#proxy-form') && (e.target.type === 'radio' || e.target.tagName === 'SELECT')) { showKind(); clearTimeout(timer); save(); }
  });
  document.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.matches('[data-copy]')) {
      navigator.clipboard.writeText(R.text).then(() => toast('copied'), () => toast('copy failed: select the text and copy it', true));
    } else if (b.matches('[data-download]')) {
      const a = document.createElement('a');
      a.href = URL.createObjectURL(new Blob([R.text], { type: 'text/plain' })); a.download = R.filename; a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 1000);
    }
  });
  api('GET', '/api/proxy').then(show).catch(e => toast(e.message, true));
})();
