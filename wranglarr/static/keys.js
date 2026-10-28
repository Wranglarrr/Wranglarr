// API keys: every app's key, hidden until asked for, with copy buttons.
(() => {
  const { api, toast, ask, esc } = W;
  const table = document.getElementById('keys'); if (!table) return;
  const KEYS = {};
  const MASK = '••••••••••••••••••••••••••••••••';

  // the clipboard API needs https (or localhost); on plain http, the old way
  async function copy(text) {
    try { await navigator.clipboard.writeText(text); return true; } catch (e) {}
    const t = document.createElement('textarea'); t.value = text; t.setAttribute('readonly', '');
    t.style.position = 'fixed'; t.style.opacity = '0'; document.body.appendChild(t); t.select();
    let ok = false; try { ok = document.execCommand('copy'); } catch (e) {}
    t.remove(); return ok;
  }

  const row = name => table.querySelector(`tr[data-key-app="${CSS.escape(name)}"]`);
  function show(tr, on) {
    const k = KEYS[tr.dataset.keyApp]; if (!k || !k.key) return;
    tr.querySelector('.key-val').textContent = on ? k.key : MASK;
    tr.querySelector('[data-key-toggle]').textContent = on ? 'Hide' : 'Show';
    tr.dataset.shown = on ? '1' : '';
  }

  // wranglarr's own keys: only their names (it keeps hashes); a new one is shown once
  function own(w) {
    const box = table.querySelector('.own-keys'); if (!box || !w || !w.auth) return;
    box.textContent = w.keys.length ? 'keys: ' + w.keys.map(k => k.name || 'unnamed').join(', ') : 'no key yet';
  }
  async function newKey() {
    const name = await ask({ title: 'A new wranglarr API key', body: 'What will use it?', ok: 'Next',
      input: { placeholder: 'e.g. homepage, home-assistant', required: true } });
    if (!name) return;
    const password = await ask({ title: 'A new wranglarr API key', body: 'Confirm with your password.', ok: 'Create',
      input: { type: 'password', autocomplete: 'current-password', required: true } });
    if (password === null) return;
    let k;
    try { k = await api('POST', '/api/auth/api-keys', { name, password }); } catch (e) { toast(e.message, true); return; }
    const tr = table.querySelector('tr[data-own]');
    tr.querySelector('.key-cell').innerHTML = `<code class="key-val">${esc(k.key)}</code>
      <div class="warn-text small">copy it now: it won't be shown again</div>`;
    tr.querySelector('td.nowrap').innerHTML = '<button class="btn ghost small" data-own-copy>Copy</button>';
    tr.dataset.key = k.key;
  }

  async function load() {
    let r;
    try { r = await api('GET', '/api/keys'); } catch (e) { toast('API keys: ' + e.message, true); return; }
    if (!table.querySelector('tr[data-own][data-key]')) own(r.wranglarr);
    const sources = new Set();
    for (const a of r.apps) {
      KEYS[a.name] = a; const tr = row(a.name); if (!tr) continue;
      const val = tr.querySelector('.key-val'), btns = tr.querySelectorAll('[data-key-toggle], [data-key-copy]');
      if (a.key) {
        val.classList.remove('muted', 'warn-text'); val.title = 'from ' + a.source; sources.add(a.source);
        btns.forEach(b => b.disabled = false); show(tr, !!tr.dataset.shown);
      } else {
        val.textContent = a.error || 'no key'; val.classList.add('warn-text'); btns.forEach(b => b.disabled = true);
      }
    }
  }

  document.addEventListener('click', async e => {
    const b = e.target.closest('button'); if (!b) return;
    const tr = b.closest('tr');
    if (b.matches('[data-own-new]')) newKey();
    else if (b.matches('[data-own-copy]')) { const ok = await copy(tr.dataset.key); toast(ok ? "wranglarr's key copied" : 'copy failed: select it and copy', !ok); }
    else if (b.matches('[data-key-toggle]')) show(tr, !tr.dataset.shown);
    else if (b.matches('[data-key-copy]')) {
      const k = KEYS[tr.dataset.keyApp];
      if (!k || !k.key) return;
      const ok = await copy(k.key);
      toast(ok ? `${tr.dataset.keyApp}'s key copied` : 'copy failed: press Show and copy it', !ok);
    } else if (b.matches('[data-copy-url]')) { const ok = await copy(b.dataset.copyUrl); toast(ok ? 'address copied' : 'copy failed', !ok); }
    else if (b.matches('[data-keys-show]')) {
      const rows = [...table.querySelectorAll('tr[data-key-app]')];
      const on = rows.some(r => !r.dataset.shown);
      rows.forEach(r => show(r, on));
      document.querySelector('[data-keys-show-label]').textContent = on ? 'Hide all' : 'Show all';
    }
  });

  // a key changed in an app: show the new one
  document.addEventListener('wranglarr:event', e => { if (['keys', 'hello', 'resync'].includes(e.detail.type)) load(); });
  load();
})();
