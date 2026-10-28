/* wranglarr: sign-in, first-run setup and the Account page (passwords, TOTP, passkeys, API keys, sessions) */
(() => {
  const { api, esc, toast, ask } = W;
  const $ = (s, el = document) => el.querySelector(s);

  // ---- passkey plumbing (WebAuthn wants ArrayBuffers; the server speaks base64url JSON) ----
  const enc = buf => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  const dec = s => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4)), c => c.charCodeAt(0)).buffer;
  const toCreate = o => ({ ...o, challenge: dec(o.challenge), user: { ...o.user, id: dec(o.user.id) },
    excludeCredentials: (o.excludeCredentials || []).map(c => ({ ...c, id: dec(c.id) })) });
  const toGet = o => ({ ...o, challenge: dec(o.challenge), allowCredentials: (o.allowCredentials || []).map(c => ({ ...c, id: dec(c.id) })) });
  function credJSON(c) {
    const r = c.response;
    const out = { id: c.id, rawId: enc(c.rawId), type: c.type, authenticatorAttachment: c.authenticatorAttachment || null,
      clientExtensionResults: c.getClientExtensionResults ? c.getClientExtensionResults() : {}, response: { clientDataJSON: enc(r.clientDataJSON) } };
    if (r.attestationObject) {
      out.response.attestationObject = enc(r.attestationObject);
      out.response.transports = r.getTransports ? r.getTransports() : [];
    } else {
      out.response.authenticatorData = enc(r.authenticatorData);
      out.response.signature = enc(r.signature);
      out.response.userHandle = r.userHandle ? enc(r.userHandle) : null;
    }
    return out;
  }
  const isIP = h => /^[\d.]+$/.test(h) || h.includes(':') || h.startsWith('[');
  function passkeyBlocker() {
    if (!window.PublicKeyCredential) return 'This browser doesn\'t support passkeys.';
    if (isIP(location.hostname)) return `Passkeys need a host name, not an IP address (${location.hostname}). Open wranglarr through a name, over HTTPS.`;
    if (!window.isSecureContext) return 'Passkeys need HTTPS. Open wranglarr through an HTTPS address (a reverse proxy, or Tailscale), or on this machine at http://localhost.';
    return '';
  }
  const cancelled = e => e && (e.name === 'NotAllowedError' || e.name === 'AbortError');

  function show(el, msg, ok) { el.hidden = !msg; el.textContent = msg || ''; el.classList.toggle('ok', !!ok); }

  // ---- a password prompt (never window.prompt: that shows the password) ----
  const askPassword = (title, why) => ask({ title, body: why || 'Confirm with your password.', ok: 'Continue',
    input: { type: 'password', autocomplete: 'current-password', required: true } });
  function showCodes(codes) {
    const d = document.createElement('dialog');
    d.className = 'dlg';
    const txt = codes.join('\n');
    d.innerHTML = `<form method="dialog"><h3>Recovery codes</h3>
      <p class="small">Each one signs you in once without your authenticator app. Save them somewhere safe now: <b>they won't be shown again</b>.</p>
      <pre class="codes">${esc(txt)}</pre>
      <div class="row"><button class="btn" type="button" data-copy>Copy</button><button class="btn" type="button" data-dl>Download</button>
      <button class="btn primary" value="ok">I've saved them</button></div></form>`;
    document.body.append(d);
    $('[data-copy]', d).onclick = () => navigator.clipboard.writeText(txt).then(() => toast('copied'), () => toast('select and copy them', true));
    $('[data-dl]', d).onclick = () => { const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob([txt + '\n'], { type: 'text/plain' }));
      a.download = 'wranglarr-recovery-codes.txt'; a.click(); };
    d.addEventListener('close', () => d.remove());
    d.showModal();
  }

  // ---- first run ----
  const setup = $('#setup-form');
  if (setup) setup.addEventListener('submit', async e => {
    e.preventDefault();
    const f = new FormData(setup), msg = $('.auth-msg', setup);
    if (f.get('password') !== f.get('password2')) return show(msg, 'The passwords don\'t match.');
    try {
      await api('POST', '/api/auth/setup', { code: f.get('code'), username: f.get('username'), password: f.get('password') });
      location.href = '/account?welcome=1';
    } catch (err) { show(msg, err.message); }
  });

  // ---- sign in ----
  // Where to go once signed in. The server checked it (safe_next); check it again the way the
  // browser reads it: no control characters or backslashes (the URL parser drops tabs and reads
  // \ as /), a path stays on this origin, and a full URL (AuthTunnel's way back) is http(s).
  const safeNext = n => {
    if (!n || /[\u0000-\u001f\u007f-\u009f\\]/.test(n)) return '/';
    try {
      if (n.startsWith('/') && !n.startsWith('//')) return new URL(n, location.origin).origin === location.origin ? n : '/';
      const u = new URL(n);
      return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : '/';
    } catch (e) { return '/'; }
  };
  const login = $('#login-form');
  if (login) {
    const next = safeNext(login.dataset.next), totp = $('#totp-form');
    login.addEventListener('submit', async e => {
      e.preventDefault();
      const f = new FormData(login), msg = $('.auth-msg', login);
      try {
        const r = await api('POST', '/api/auth/login', { username: f.get('username'), password: f.get('password'), remember: !!f.get('remember') });
        if (r.mfa) { login.hidden = true; $('#passkey-box').hidden = true; totp.hidden = false; $('input', totp).focus(); return; }
        location.href = next;
      } catch (err) { show(msg, err.message); }
    });
    totp.addEventListener('submit', async e => {
      e.preventDefault();
      const msg = $('.auth-msg', totp);
      try { await api('POST', '/api/auth/login/totp', { code: new FormData(totp).get('code') }); location.href = next; }
      catch (err) { show(msg, err.message); if (/expired|start again/.test(err.message)) setTimeout(() => location.reload(), 1500); }
    });
    $('[data-restart]', totp).onclick = () => location.reload();

    const box = $('#passkey-box'), why = $('#passkey-why'), btn = $('#passkey-login');
    const block = passkeyBlocker();
    if (block && !box.hidden) { btn.disabled = true; why.hidden = false; why.textContent = block; }
    btn.addEventListener('click', async () => {
      const msg = $('.auth-msg', login);
      try {
        const o = await api('GET', '/api/auth/passkey/login-options');
        const cred = await navigator.credentials.get({ publicKey: toGet(o.options) });
        await api('POST', '/api/auth/passkey/login', { flow: o.flow, credential: credJSON(cred), remember: !!new FormData(login).get('remember') });
        location.href = next;
      } catch (err) { if (!cancelled(err)) show(msg, err.message); }
    });
  }

  // ---- account page ----
  const acc = $('#account');
  if (!acc) return;
  const when = s => s ? W.fmtTs ? W.fmtTs(s) : new Date(s).toLocaleString() : 'never';
  let me = {};

  async function load() {
    me = await api('GET', '/api/auth/me');
    // two-factor
    const t = $('#totp-state');
    t.innerHTML = me.totp
      ? `<p><span class="pill pill-ok">on</span> Signing in with the password also asks for a code from your authenticator app.</p>
         <p class="muted small">${me.recovery_left} unused recovery code${me.recovery_left === 1 ? '' : 's'} left.</p>
         <div class="row"><button class="btn" data-recovery>New recovery codes</button><button class="btn ghost danger" data-totp-off>Turn off</button></div>`
      : `<p><span class="pill">off</span> Add a second step to password sign-ins: a 6-digit code from an authenticator app
         (Aegis, 2FAS, Google Authenticator, 1Password, Bitwarden…).</p>
         <div class="row"><button class="btn primary" data-totp-on>Turn on</button></div><div id="totp-setup"></div>`;
    // passkeys
    const pk = me.passkeys || [];
    $('#passkey-list').innerHTML = pk.length ? `<table class="compact"><thead><tr><th>Name</th><th>Site</th><th>Added</th><th>Last used</th><th>Synced</th><th></th></tr></thead><tbody>${
      pk.map(p => `<tr><td><b>${esc(p.name)}</b></td><td><code>${esc(p.rp_id)}</code></td><td class="small">${when(p.created)}</td><td class="small">${when(p.last_used)}</td>
        <td class="small">${p.backed_up ? 'yes' : 'no'}</td><td><button class="btn ghost small" data-pk-rename="${esc(p.id)}">Rename</button>
        <button class="btn ghost danger small" data-pk-del="${esc(p.id)}">Remove</button></td></tr>`).join('')}</tbody></table>`
      : '<p class="muted">No passkeys yet.</p>';
    const block = passkeyBlocker();
    $('#passkey-warn').innerHTML = block ? `<div class="check warn">${esc(block)}</div>` : '';
    $('#passkey-add').disabled = !!block;
    // api keys
    const ks = me.api_keys || [];
    $('#key-list').innerHTML = ks.length ? `<table class="compact"><thead><tr><th>Name</th><th>Starts with</th><th>Created</th><th>Last used</th><th></th></tr></thead><tbody>${
      ks.map(k => `<tr><td><b>${esc(k.name)}</b></td><td><code>${esc(k.prefix)}…</code></td><td class="small">${when(k.created)}</td><td class="small">${when(k.last_used)}</td>
        <td><button class="btn ghost danger small" data-key-del="${esc(k.id)}">Revoke</button></td></tr>`).join('')}</tbody></table>`
      : '<p class="muted">No API keys.</p>';
    // sessions
    $('#session-list').innerHTML = `<table class="compact"><thead><tr><th>Browser</th><th>From</th><th>How</th><th>Signed in</th><th>Last seen</th><th></th></tr></thead><tbody>${
      (me.sessions || []).map(s => `<tr><td class="small">${esc(s.agent || '?')}${s.current ? ' <span class="pill pill-ok">this one</span>' : ''}</td><td>${esc(s.ip || '')}</td>
        <td class="small">${esc(s.method || '')}</td><td class="small">${when(s.created)}</td><td class="small">${when(s.last_seen)}</td>
        <td>${s.current ? '' : `<button class="btn ghost danger small" data-sess-del="${esc(s.id)}">Sign out</button>`}</td></tr>`).join('')}</tbody></table>`;
  }

  const run = fn => async (...a) => { try { await fn(...a); } catch (err) { if (!cancelled(err)) toast(err.message, true); } };

  $('#pw-form').addEventListener('submit', run(async e => {
    e.preventDefault();
    const f = new FormData(e.target);
    if (f.get('new') !== f.get('new2')) throw new Error('the new passwords don\'t match');
    const r = await api('POST', '/api/auth/password', { current: f.get('current'), new: f.get('new') });
    e.target.reset();
    toast(r.ended ? `password changed; ${r.ended} other session${r.ended === 1 ? '' : 's'} signed out` : 'password changed');
    await load();
  }));

  acc.addEventListener('click', run(async e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b.matches('[data-totp-on]')) {
      const pw = await askPassword('Turn on two-factor'); if (pw === null) return;
      const r = await api('POST', '/api/auth/totp/begin', { password: pw });
      $('#totp-setup').innerHTML = `<div class="totp-setup"><div class="qr">${r.qr_svg}</div><div>
        <p class="small">1. Scan this with your authenticator app, or type the key:</p><p><code class="secret">${esc(r.secret)}</code></p>
        <p class="small">2. Enter the 6-digit code it shows:</p>
        <div class="row"><input id="totp-code" inputmode="numeric" autocomplete="one-time-code" placeholder="123456" maxlength="7">
        <button class="btn primary" data-totp-confirm>Confirm</button></div></div></div>`;
      $('#totp-code').focus();
    } else if (b.matches('[data-totp-confirm]')) {
      const r = await api('POST', '/api/auth/totp/enable', { code: $('#totp-code').value });
      await load(); showCodes(r.recovery);
    } else if (b.matches('[data-totp-off]')) {
      const pw = await askPassword('Turn off two-factor', 'Password sign-ins will no longer ask for a code. Your recovery codes stop working.'); if (pw === null) return;
      await api('POST', '/api/auth/totp/disable', { password: pw }); await load(); toast('two-factor is off');
    } else if (b.matches('[data-recovery]')) {
      const pw = await askPassword('New recovery codes', 'The old codes stop working.'); if (pw === null) return;
      const r = await api('POST', '/api/auth/recovery', { password: pw }); await load(); showCodes(r.recovery);
    } else if (b.id === 'passkey-add') {
      const pw = await askPassword('Add a passkey'); if (pw === null) return;
      const o = await api('POST', '/api/auth/passkey/register-options', { password: pw });
      const cred = await navigator.credentials.create({ publicKey: toCreate(o.options) });
      await api('POST', '/api/auth/passkey/register', { flow: o.flow, credential: credJSON(cred), name: $('#passkey-name').value });
      $('#passkey-name').value = ''; await load(); toast('passkey added');
    } else if (b.dataset.pkRename) {
      const p = (me.passkeys || []).find(x => x.id === b.dataset.pkRename);
      const name = await ask({ title: 'Rename this passkey', ok: 'Rename', input: { value: p ? p.name : '', placeholder: 'e.g. phone, laptop', required: true } });
      if (!name) return;
      await api('PATCH', `/api/auth/passkeys/${encodeURIComponent(b.dataset.pkRename)}`, { name }); await load();
    } else if (b.dataset.pkDel) {
      if (!await ask({ title: 'Remove this passkey?', body: 'It will no longer sign you in to wranglarr.', ok: 'Remove', danger: true })) return;
      await api('DELETE', `/api/auth/passkeys/${encodeURIComponent(b.dataset.pkDel)}`); await load();
    } else if (b.id === 'key-add') {
      const pw = await askPassword('Create an API key'); if (pw === null) return;
      const k = await api('POST', '/api/auth/api-keys', { name: $('#key-name').value, password: pw });
      $('#key-name').value = '';
      const box = $('#key-new'); box.hidden = false;
      box.innerHTML = `<div class="check ok">New key <b>${esc(k.name)}</b>. Copy it now, it won't be shown again:<br><code class="secret">${esc(k.key)}</code>
        <button class="btn small" data-copy-key>Copy</button></div>`;
      $('[data-copy-key]', box).onclick = () => navigator.clipboard.writeText(k.key).then(() => toast('copied'));
      await load();
    } else if (b.dataset.keyDel) {
      if (!await ask({ title: 'Revoke this API key?', body: 'Anything using it stops working.', ok: 'Revoke', danger: true })) return;
      await api('DELETE', `/api/auth/api-keys/${encodeURIComponent(b.dataset.keyDel)}`); await load();
    } else if (b.dataset.sessDel) {
      await api('DELETE', `/api/auth/sessions/${encodeURIComponent(b.dataset.sessDel)}`); await load();
    } else if (b.id === 'end-others') {
      const r = await api('POST', '/api/auth/sessions/end-others'); await load(); toast(`${r.ended} other session(s) signed out`);
    }
  }));
  $('#sign-out').addEventListener('click', run(async () => { await api('POST', '/api/auth/logout'); location.href = '/login'; }));

  load().then(() => {
    if (new URLSearchParams(location.search).get('welcome')) toast('Account created. Turn on two-factor or add a passkey below.');
  }).catch(err => toast(err.message, true));
})();
