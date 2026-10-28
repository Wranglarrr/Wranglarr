/* wranglarr first-run wizard */
(async () => {
  const { api, esc, fmtBytes, toast } = W;
  const D = await api('GET', '/api/setup/defaults');
  const E = D.existing_env || {};
  const S = {   // answers
    services: (E.COMPOSE_PROFILES || 'radarr,sonarr,lidarr,bazarr').split(',').filter(Boolean),
    tz: E.TZ || D.host.tz, puid: +(E.PUID || D.host.uid), pgid: +(E.PGID || D.host.gid), umask: E.UMASK || '002',
    appdata: E.APPDATA || './appdata', data_root: E.DATA_ROOT || '/mnt/data', data_root_2: E.DATA_ROOT_2 || '',
    postgres_user: E.POSTGRES_USER || 'arr', postgres_password: E.POSTGRES_PASSWORD || '',
    ports: Object.fromEntries(D.services.map(s => [s.id, +(E[s.id.toUpperCase() + '_PORT'] || s.port)])),
    wranglarr_port: +(E.WRANGLARR_PORT || 8787), gluetun_api_key: E.GLUETUN_API_KEY || '',
    nzbget_user: E.NZBGET_USER || 'nzbget', nzbget_pass: E.NZBGET_PASS || '',
    qbittorrent_user: E.QBITTORRENT_USER || 'admin', qbittorrent_password: E.QBITTORRENT_PASSWORD || '',
    // app logins: an existing .env without APPS_AUTH has been manual all along
    apps_auth: E.APPS_AUTH || (D.env_exists ? 'manual' : 'apps'), apps_login_user: E.APPS_LOGIN_USER || 'admin',
    apps_login_password: E.APPS_LOGIN_PASSWORD || '', apps_auth_required: E.APPS_AUTH_REQUIRED || 'enabled',
    auth_cookie_domain: E.AUTH_COOKIE_DOMAIN || '', auth_login_url: E.AUTH_LOGIN_URL || '',
    vpn: { mode: (E.COMPOSE_FILE || '').includes('per-app') ? 'per-app' : (E.COMPOSE_FILE || '').includes('vpn') ? 'shared' : 'none',
      scope: !(E.COMPOSE_FILE || '').includes('compose.vpn.yaml') || (E.COMPOSE_FILE || '').includes('vpn-all') ? 'all'
        : (E.COMPOSE_FILE || '').includes('vpn-prowlarr') ? 'prowlarr' : 'downloads',
      vpn_type: E.VPN_TYPE || 'wireguard', provider: E.VPN_SERVICE_PROVIDER || 'custom',
      private_key: E.WIREGUARD_PRIVATE_KEY || '', preshared_key: E.WIREGUARD_PRESHARED_KEY || '', public_key: E.WIREGUARD_PUBLIC_KEY || '',
      endpoint_ip: E.WIREGUARD_ENDPOINT_IP || '', endpoint_port: E.WIREGUARD_ENDPOINT_PORT || '51820', addresses: E.WIREGUARD_ADDRESSES || '',
      lan_network: E.VPN_LAN_NETWORK || '', user: E.VPN_USER || '', password: E.VPN_PASSWORD || '' },
    vpn_per_app: D.services.filter(s => (E[s.id.toUpperCase() + '_VPN'] || '') === 'true').map(s => s.id),
    backup_cron: '30 3 * * *', keep_last: 14, keep_days: 60,
  };
  const R = { versions: [], legacy: Object.create(null) };        // restore choices
  let scan = null, written = null;

  const STEPS = D.auth_only ? [
    { id: 'logins', title: 'App logins', render: renderLogins, validate: validateLogins },
    { id: 'review', title: 'Review & write', render: renderReviewAuth },
    { id: 'done', title: 'Next steps', render: renderDone },
  ] : [
    { id: 'services', title: 'Services', render: renderServices, validate: () => S.services.length ? null : 'pick at least one service' },
    { id: 'system', title: 'System', render: renderSystem },
    { id: 'storage', title: 'Storage', render: renderStorage, validate: () => S.data_root ? null : 'DATA_ROOT is required' },
    { id: 'creds', title: 'Credentials', render: renderCreds, validate: () => S.postgres_password ? null : 'generate or enter a Postgres password' },
    { id: 'logins', title: 'App logins', render: renderLogins, validate: validateLogins },
    { id: 'network', title: 'Network & VPN', render: renderNetwork, validate: validateVpn },
    { id: 'backups', title: 'Existing backups', render: renderBackups },
    { id: 'review', title: 'Review & write', render: renderReview },
    { id: 'done', title: 'Next steps', render: renderDone },
  ];
  let cur = 0;

  const $ = sel => document.querySelector(sel);
  const body = $('#step-body');
  const field = (label, inner, hint) => `<div class="form-row"><label class="l">${label}</label><div>${inner}${hint ? `<div class="hint">${hint}</div>` : ''}</div></div>`;
  const inp = (k, v, attrs = '') => `<input data-k="${k}" value="${esc(v ?? '')}" ${attrs}>`;
  const sel = (k, v, opts) => `<select data-k="${k}">${opts.map(o => `<option value="${o[0]}" ${o[0] == v ? 'selected' : ''}>${o[1]}</option>`).join('')}</select>`;
  // a check line: its message is text (paths, server errors), never markup
  const chk = s => `<div class="check ${esc(s.cls)}">${esc(s.msg)}</div>`;
  // an element built with DOM APIs, so what a backup's manifest says stays text
  const el = (tag, props = {}, ...kids) => {
    const e = Object.assign(document.createElement(tag), props);
    e.append(...kids.filter(k => k != null && k !== ''));
    return e;
  };
  const get = (obj, path) => path.split('.').reduce((o, k) => o[k], obj);
  const set = (obj, path, val) => { const ks = path.split('.'); const last = ks.pop(); ks.reduce((o, k) => o[k], obj)[last] = val; };

  function bind() {
    body.querySelectorAll('[data-k]').forEach(el => el.addEventListener('input', () => {
      let v = el.type === 'checkbox' ? el.checked : el.value;
      if (el.type === 'number') v = +v;
      set(S, el.dataset.k, v);
    }));
  }

  // ---- steps -----------------------------------------------------------------
  function renderServices() {
    const groups = { media: 'Media managers (on Postgres)', download: 'Download clients', requests: 'Requests (optional)' };
    let h = '<h2>Which services do you want?</h2><p class="muted small">Postgres and wranglarr always run. This becomes COMPOSE_PROFILES in .env; you can change it later.</p>';
    for (const [g, title] of Object.entries(groups)) {
      h += `<h3>${title}</h3><div class="svc-grid">`;
      for (const s of D.services.filter(x => x.group === g)) {
        const on = S.services.includes(s.id);
        h += `<label class="svc ${on ? 'on' : ''}"><input type="checkbox" data-svc="${s.id}" ${on ? 'checked' : ''}><span><b>${s.name}</b><span class="muted small">${s.desc} · :${S.ports[s.id]}${s.db ? ' · Postgres' : ' · config files'}</span></span></label>`;
      }
      h += '</div>';
    }
    body.innerHTML = h;
    body.querySelectorAll('[data-svc]').forEach(el => el.addEventListener('change', () => {
      const id = el.dataset.svc; S.services = D.services.map(s => s.id).filter(x => x === id ? el.checked : S.services.includes(x));
      el.closest('.svc').classList.toggle('on', el.checked);
    }));
  }

  function renderSystem() {
    const users = (D.host.users || []).map(u => [esc(`${u.uid}:${u.gid}`), esc(`${u.name} (${u.uid}:${u.gid} ${u.group})`)]);
    const pick = users.length ? `<select id="user-pick"><option value="">custom…</option>${users.map(u => `<option value="${u[0]}" ${u[0] === S.puid + ':' + S.pgid ? 'selected' : ''}>${u[1]}</option>`).join('')}</select>` : '';
    body.innerHTML = '<h2>System</h2>' +
      field('Timezone', inp('tz', S.tz), D.in_docker ? 'IANA name, e.g. America/Chicago (pass -e TZ=… to docker run to prefill)' : `host reports ${esc(D.host.tz)}`) +
      field('Run containers as', `${pick}<div class="inline" style="margin-top:6px">PUID ${inp('puid', S.puid, 'type="number" min="0"')} PGID ${inp('pgid', S.pgid, 'type="number" min="0"')}</div>`,
        'the user that owns appdata and your media. Files the apps create get this owner. ' + (D.in_docker ? 'On Linux run <code>id</code> on the host; the Storage step shows who owns the mounted drive.' : '') +
        (D.host.root ? ' TrueNAS SCALE: 568:568 is the built-in apps user; use it with datasets that have the Apps ACL preset, or your own user\'s uid.' : '')) +
      field('UMASK', inp('umask', S.umask), '002 = group-writable files (recommended for shared media)') +
      '<div id="root-warn"></div>';
    bind();
    const rootWarn = () => { const w = $('#root-warn'); if (w) w.innerHTML = (+S.puid === 0 || +S.pgid === 0) ? chk({ cls: 'bad',
      msg: 'PUID/PGID 0 runs every app as root: the files they create on your media are root-owned and the apps can change anything. Use a normal user and a group your media is shared with.' }) : ''; };
    body.addEventListener('input', e => { if (e.target.dataset && /^p[ug]id$/.test(e.target.dataset.k || '')) setTimeout(rootWarn); });
    rootWarn();
    const up = $('#user-pick'); if (up) up.addEventListener('change', e => { if (!e.target.value) return; const [u, g] = e.target.value.split(':'); S.puid = +u; S.pgid = +g;
      body.querySelector('[data-k=puid]').value = u; body.querySelector('[data-k=pgid]').value = g; });
  }

  async function checkPath(k, create) {
    const out = body.querySelector(`[data-check="${k}"]`); out.innerHTML = '<div class="check">checking…</div>';
    try {
      const r = await api('POST', '/api/setup/check-path', { path: S[k], key: k, uid: S.puid, gid: S.pgid, create: !!create });
      if (r.skipped) return out.innerHTML = '';
      if (r.error) return out.innerHTML = chk({ cls: 'bad', msg: r.error });
      if (r.unverified) return out.innerHTML = chk({ cls: 'warn', msg: r.note });
      const where = r.via === 'mount' ? ` (checked via the ${r.checked} mount)` : '';
      if (!r.exists) return out.innerHTML = chk({ cls: 'warn', msg: `${r.path} does not exist${where}` }) + `<button class="btn small" data-mk="${esc(k)}">create it</button>`;
      const rw = r.writable ? 'writable' : (r.readable ? 'read-only' : 'NOT accessible');
      out.innerHTML = chk({ cls: r.writable ? 'ok' : 'bad', msg: `${r.path}${where} · owner ${r.owner_uid}:${r.owner_gid} mode ${r.mode} · ${rw} by ${S.puid}:${S.pgid}` +
        (r.free_bytes !== undefined ? ` · ${fmtBytes(r.free_bytes)} free of ${fmtBytes(r.total_bytes)}` : '') + (r.mount ? ' · mount point' : '') + (r.created ? ' · created' : '') + (r.note ? ' · ' + r.note : '') }) + (r.notes || []).map(n => chk({ cls: n.cls, msg: n.msg })).join('');
    } catch (e) { out.innerHTML = chk({ cls: 'bad', msg: e.message }); }
  }
  function renderStorage() {
    const dockerNote = D.in_docker ? `<p class="muted small">Type the <b>host</b> paths; they are written to .env as-is. Checks run against what is mounted into this container: <code>/stack</code> (the stack dir${D.mounts.data_root ? '' : ''}), <code>/data</code>${D.mounts.data_root ? ' ✓' : ' (not mounted — add -v &lt;DATA_ROOT&gt;:/data to verify)'}, <code>/data2</code>${D.mounts.data_root_2 ? ' ✓' : ''}, <code>/appdata</code>${D.mounts.appdata ? ' ✓' : ' (only needed for an absolute APPDATA outside the stack dir)'}.</p>` : '';
    body.innerHTML = '<h2>Storage &amp; mounts</h2>' + dockerNote +
      field('APPDATA', inp('appdata', S.appdata) + ' <button class="btn small" data-pc="appdata">check</button><div data-check="appdata"></div>', 'app configs, Postgres cluster, wranglarr state and backup versions. Relative = relative to the stack dir.') +
      field('DATA_ROOT', inp('data_root', S.data_root) + ' <button class="btn small" data-pc="data_root">check</button><div data-check="data_root"></div>', 'mounted at /data in every app: downloads + media on ONE filesystem so hardlinks work (downloads/, media/movies, media/tv, media/music)') +
      field('DATA_ROOT_2', inp('data_root_2', S.data_root_2) + ' <button class="btn small" data-pc="data_root_2">check</button><div data-check="data_root_2"></div>', 'optional second drive → /data2 (uncomment its lines in compose.yaml)');
    bind();
    body.addEventListener('click', e => { const b = e.target.closest('button'); if (!b) return;
      if (b.dataset.pc) checkPath(b.dataset.pc, false); if (b.dataset.mk) checkPath(b.dataset.mk, true); });
    ['appdata', 'data_root'].forEach(k => checkPath(k));
  }

  async function gen(kind, k) { const r = await api('POST', '/api/setup/generate', { kind }); set(S, k, r.value); const el = body.querySelector(`[data-k="${k}"]`); if (el) el.value = r.value; }
  function renderCreds() {
    body.innerHTML = '<h2>Credentials</h2><p class="muted small">Generated values are written to .env (mode 600). Nothing is tested against a live service yet, there is none; the init script creates them with these values.</p>' +
      field('Postgres user', inp('postgres_user', S.postgres_user), 'owns every app database') +
      field('Postgres password', inp('postgres_password', S.postgres_password) + ' <button class="btn small" data-gen="password:postgres_password">generate</button>') +
      field('gluetun API key', inp('gluetun_api_key', S.gluetun_api_key) + ' <button class="btn small" data-gen="apikey:gluetun_api_key">generate</button>', 'lets wranglarr read VPN status from the control server (shared tunnel only)') +
      field('Backup schedule', `${inp('backup_cron', S.backup_cron)} keep last ${inp('keep_last', S.keep_last, 'type="number" min="0"')} · ${inp('keep_days', S.keep_days, 'type="number" min="0"')} days`, 'when the media suite backs up (blank = never), and how many versions every suite keeps (0 = no limit); you can change both in wranglarr later');
    bind();
    body.addEventListener('click', e => { const b = e.target.closest('[data-gen]'); if (b) { const [kind, k] = b.dataset.gen.split(':'); gen(kind, k); } });
    if (!S.postgres_password) gen('password', 'postgres_password');
    if (!S.gluetun_api_key) gen('apikey', 'gluetun_api_key');
  }

  // ---- app logins ----
  const MODES = [
    ['apps', 'One login for every app', 'init sets the same username and password in each app (the arrs, Prowlarr, Bazarr, SABnzbd, qBittorrent, NZBGet). You still sign in to each app, but it\'s always the same login.'],
    ['tunnel', 'AuthTunnel: sign in once, to wranglarr', 'The apps get no login of their own. Their ports belong to wranglarr-gate, which lets you through when you\'re signed in to wranglarr (with its two-factor and passkeys). API keys still work, so mobile apps and scripts for the arrs keep going.'],
    ['manual', 'Leave the apps\' logins to me', 'Each app asks you to create a login on first visit, the way it does without wranglarr.'],
  ];
  function renderLogins() {
    const m = S.apps_auth, proxy = !!(S.auth_cookie_domain || S.auth_login_url);
    let h = '<h2>App logins</h2><p class="muted small">How you sign in to the apps themselves. wranglarr has its own login either way. ' +
      (D.auth_only ? 'Only these settings change in .env; <code>./scripts/init.sh --auth</code> applies them.' : 'Set up by init.sh; change it later with <code>wranglarr-setup --auth</code>.') + '</p>';
    h += MODES.map(([id, t, d]) => `<label class="svc ${m === id ? 'on' : ''}" style="display:flex;margin:6px 0"><input type="radio" name="apps_auth" value="${id}" ${m === id ? 'checked' : ''}><span><b>${t}</b><span class="muted small">${d}</span></span></label>`).join('');
    if (m === 'apps') {
      h += field('Username', inp('apps_login_user', S.apps_login_user, 'autocomplete="off" spellcheck="false"')) +
        field('Password', inp('apps_login_password', S.apps_login_password, 'autocomplete="off" spellcheck="false"') + ' <button class="btn small" data-gen="password:apps_login_password">generate</button>',
          'At least 12 characters. Not your wranglarr password: SABnzbd keeps it in plain text in sabnzbd.ini, Bazarr as an unsalted MD5.') +
        field('Ask for it', sel('apps_auth_required', S.apps_auth_required, [['enabled', 'everywhere'], ['disabledForLocalAddresses', 'not on the local network (arrs and Prowlarr)']]),
          '"Not on the local network" can mean never: behind Docker\'s port forwarding every visitor can look local to the app.');
    } else if (m === 'tunnel') {
      h += field('You open the apps', `<label class="check-line"><input type="radio" name="access" value="lan" ${proxy ? '' : 'checked'}> by this machine's address and their ports, e.g. http://192.168.1.10:7878</label>
        <label class="check-line"><input type="radio" name="access" value="proxy" ${proxy ? 'checked' : ''}> through a reverse proxy, at names like https://radarr.example.com</label>`,
        'The sign-in is a cookie, and a browser only sends it to the address it came from (any port) or, when told, to every name under one domain.');
      if (proxy) h += field('Cookie domain', inp('auth_cookie_domain', S.auth_cookie_domain, 'placeholder="example.com" spellcheck="false"'), 'the domain all the apps and wranglarr live under; the cookie then reaches every *.example.com') +
        field('wranglarr\'s address', inp('auth_login_url', S.auth_login_url, 'placeholder="https://wranglarr.example.com" spellcheck="false"'), 'where the gate sends you to sign in');
      h += chk({ cls: 'warn', msg: 'qBittorrent\'s web UI has no API key, so its mobile apps can\'t get past the gate. The arrs, Prowlarr, Bazarr and SABnzbd apps use API keys and keep working.' });
    } else {
      if (S.services.includes('nzbget')) h += field('NZBGet login', `${inp('nzbget_user', S.nzbget_user, 'placeholder="user"')} ${inp('nzbget_pass', S.nzbget_pass, 'placeholder="password"')} <button class="btn small" data-gen="password:nzbget_pass">generate</button>`, 'the image applies it at every start');
      if (S.services.includes('qbittorrent')) h += field('qBittorrent login', `${inp('qbittorrent_user', S.qbittorrent_user, 'placeholder="user"')} ${inp('qbittorrent_password', S.qbittorrent_password, 'placeholder="blank: set it later"')}`,
        'Blank: qBittorrent prints a temporary password in its log on first start; set your own in its UI, then put it in .env as QBITTORRENT_PASSWORD.');
    }
    body.innerHTML = h; bind();
    body.querySelectorAll('[name=apps_auth]').forEach(el => el.addEventListener('change', async () => {
      S.apps_auth = el.value;
      if (S.apps_auth === 'tunnel') {          // wranglarr, the arrs and the gate use these; nobody types them
        if (!S.qbittorrent_password) S.qbittorrent_password = (await api('POST', '/api/setup/generate', {})).value;
        if (!S.nzbget_pass) S.nzbget_pass = (await api('POST', '/api/setup/generate', {})).value;
      }
      renderLogins();
    }));
    body.querySelectorAll('[name=access]').forEach(el => el.addEventListener('change', () => {
      if (el.value === 'lan') { S.auth_cookie_domain = ''; S.auth_login_url = ''; renderLogins(); }
      else { S.auth_login_url = S.auth_login_url || 'https://'; renderLogins(); }
    }));
    body.addEventListener('click', e => { const b = e.target.closest('[data-gen]'); if (b) { const [kind, k] = b.dataset.gen.split(':'); gen(kind, k); } });
    if (m === 'apps' && !S.apps_login_password) gen('password', 'apps_login_password');
  }
  function validateLogins() {
    if (S.apps_auth === 'apps') {
      if (!S.apps_login_user.trim()) return 'give the apps\' login a username';
      if ((S.apps_login_password || '').length < 12) return 'the apps\' password needs at least 12 characters';
    }
    if (S.apps_auth === 'tunnel') {
      if (S.auth_login_url === 'https://') S.auth_login_url = '';
      if (!!S.auth_cookie_domain !== !!S.auth_login_url) return 'reverse proxy: fill in both the cookie domain and wranglarr\'s address';
    }
    return null;
  }
  async function renderReviewAuth() {
    const p = await api('POST', '/api/setup/preview-auth', S);
    body.innerHTML = '<h2>Review</h2><p class="small">These keys change in <code>.env</code> (a .env.bak-&lt;timestamp&gt; copy is kept); nothing else does.</p>' +
      '<table class="compact"><thead><tr><th>Key</th><th>Now</th><th>New</th></tr></thead><tbody>' +
      p.changes.map(c => `<tr class="${c.same ? 'muted' : ''}"><td><code>${esc(c.key)}</code></td><td>${esc(c.old || '—')}</td><td>${c.same ? 'unchanged' : '<b>' + esc(c.new || '(blank)') + '</b>'}</td></tr>`).join('') +
      '</tbody></table>';
  }

  async function validateVpn() {
    const r = await api('POST', '/api/setup/check-vpn', S.vpn);
    const out = body.querySelector('#vpn-check');
    if (out) out.innerHTML = r.errors.map(m => chk({ cls: 'bad', msg: m })).join('') + r.warnings.map(m => chk({ cls: 'warn', msg: m })).join('');
    return r.errors.length ? r.errors[0] : null;
  }
  function renderNetwork() {
    const v = S.vpn;
    const providers = [['custom', 'custom (own keys / wg0.conf / .ovpn)'], ['mullvad', 'Mullvad'], ['protonvpn', 'ProtonVPN'], ['private internet access', 'Private Internet Access'], ['airvpn', 'AirVPN'], ['ivpn', 'IVPN'], ['nordvpn', 'NordVPN'], ['surfshark', 'Surfshark'], ['windscribe', 'Windscribe']];
    let h = '<h2>Network &amp; VPN</h2>' +
      field('VPN', sel('vpn.mode', v.mode, [['none', 'none'], ['shared', 'one shared tunnel (gluetun)'], ['per-app', 'a tunnel per app (hotio images)']]), 'shared: one WireGuard/OpenVPN peer, kill switch, UIs still reachable on the LAN') +
      field('Ports', D.services.filter(s => S.services.includes(s.id)).map(s => `${s.name} ${inp('ports.' + s.id, S.ports[s.id], 'type="number"')}`).join(' ') + ` wranglarr ${inp('wranglarr_port', S.wranglarr_port, 'type="number"')}`, 'published on the host');
    if (v.mode === 'shared') {
      const hasP = S.services.includes('prowlarr');
      if (!hasP && v.scope === 'prowlarr') v.scope = 'all';
      h += field('In the tunnel', sel('vpn.scope', v.scope, [['all', 'every app'], ...(hasP ? [['prowlarr', 'download clients and Prowlarr']] : []), ['downloads', 'download clients only']]),
        v.scope === 'prowlarr' ? 'the arrs use your own connection and reach indexers only through Prowlarr: add every indexer in Prowlarr, with Redirect off. The dashboard warns about any that go around it.'
        : v.scope === 'downloads' ? 'only qBittorrent, SABnzbd and NZBGet use the tunnel. The arrs and Prowlarr search indexers on your own connection.'
        : 'the simplest: nothing the apps do leaves without the tunnel (Seerr stays outside: it only talks to your own apps and metadata servers)') +
        field('Protocol', sel('vpn.vpn_type', v.vpn_type, [['wireguard', 'WireGuard'], ['openvpn', 'OpenVPN (legacy)']])) +
        field('Provider', sel('vpn.provider', v.provider, providers)) +
        field('LAN subnets', inp('vpn.lan_network', v.lan_network, 'placeholder="192.168.1.0/24"'), 'optional: LAN ranges the apps may reach directly (NAS, download client on another box). The Docker network is always allowed.');
      if (v.vpn_type === 'wireguard') {
        h += field('Private key', inp('vpn.private_key', v.private_key, 'type="password"'), v.provider === 'custom' ? 'leave blank to use appdata/vpn/wireguard/wg0.conf instead' : 'from your provider\'s WireGuard config') +
          field('Tunnel address', inp('vpn.addresses', v.addresses, 'placeholder="10.2.0.2/32"'), 'WIREGUARD_ADDRESSES');
        if (v.provider === 'custom') h += field('Peer public key', inp('vpn.public_key', v.public_key)) +
          field('Endpoint', `${inp('vpn.endpoint_ip', v.endpoint_ip, 'placeholder="IP address"')} : ${inp('vpn.endpoint_port', v.endpoint_port, 'type="number"')}`, 'must be an IP, gluetun does not resolve hostnames') +
          field('Preshared key', inp('vpn.preshared_key', v.preshared_key, 'type="password"'), 'optional');
      }
      if (v.vpn_type === 'openvpn' || ['protonvpn', 'private internet access'].includes(v.provider))
        h += field('Account login', `${inp('vpn.user', v.user, 'placeholder="username"')} ${inp('vpn.password', v.password, 'type="password" placeholder="password"')}`, v.vpn_type === 'openvpn' ? 'OpenVPN credentials from the provider' : 'needed for port forwarding');
      if (v.vpn_type === 'openvpn' && v.provider === 'custom') h += field('Custom .ovpn', '<span class="muted small">place it at appdata/vpn/custom.conf after setup (remote = an IP) and set OPENVPN_CUSTOM_CONFIG=/gluetun/custom.conf in .env</span>');
    }
    if (v.mode === 'per-app') {
      h += field('Apps on VPN', D.services.filter(s => S.services.includes(s.id) && s.vpn !== false).map(s => `<label class="inline"><input type="checkbox" data-pa="${s.id}" ${S.vpn_per_app.includes(s.id) ? 'checked' : ''}> ${s.name}</label>`).join(' '), 'each needs its own WireGuard peer at appdata/<app>/wireguard/wg0.conf') +
        field('LAN subnet', inp('vpn.lan_network', v.lan_network, 'placeholder="192.168.1.0/24"'), 'required by hotio to allow web UI access from your LAN');
    }
    h += '<div id="vpn-check"></div>';
    body.innerHTML = h; bind();
    body.querySelectorAll('[data-k^="vpn."]').forEach(el => { if (el.tagName === 'SELECT') el.addEventListener('change', renderNetwork); });
    body.querySelectorAll('[data-pa]').forEach(el => el.addEventListener('change', () => { S.vpn_per_app = D.services.map(s => s.id).filter(x => x === el.dataset.pa ? el.checked : S.vpn_per_app.includes(x)); }));
  }

  async function renderBackups() {
    body.innerHTML = '<h2>Existing backups</h2><p class="muted">scanning ' + esc(S.appdata) + '…</p>';
    try { scan = await api('POST', '/api/setup/scan-backups', { appdata: S.appdata }); } catch (e) { body.innerHTML += chk({ cls: 'bad', msg: e.message }); return; }
    body.replaceChildren(el('h2', { textContent: 'Existing backups' }));
    const add = (...n) => body.append(...n);
    if (!scan.versions.length && !scan.legacy.length && !scan.configs.length) add(el('p', { className: 'muted', textContent: `Nothing found under ${S.appdata}. Fresh install.` }));
    if (scan.configs.length) add(el('p', { className: 'check ok', textContent: `Existing app configs found for: ${scan.configs.join(', ')} — init.sh will keep them and only add the Postgres settings.` }));
    if (scan.sqlite.length) add(el('p', { className: 'check warn', textContent: `SQLite databases found for: ${scan.sqlite.join(', ')} — the data must be migrated with pgloader after first start (next steps will say how).` }));
    if (scan.versions.length) {
      const rows = el('tbody');
      const heads = ['', 'Suite', 'Version', 'Status', 'Apps', 'Size'].map(t => el('th', { textContent: t }));
      heads[5].className = 'num';
      add(el('h3', { textContent: 'wranglarr versions' }),
        el('p', { className: 'muted small', textContent: 'Tick what to restore after install. wranglarr will show a one-click restore plan once the apps are up.' }),
        el('table', { className: 'compact' }, el('thead', {}, el('tr', {}, ...heads)), rows));
      for (const v of scan.versions) for (const [app, a] of Object.entries(v.apps)) {
        const key = `${v.suite}|${v.version}|${app}`;
        const usable = (a.databases || a.config) && S.services.includes(app);
        const box = el('input', { type: 'checkbox', checked: R.versions.some(x => x.key === key), disabled: !usable });
        box.addEventListener('change', () => {
          R.versions = R.versions.filter(x => x.key !== key);
          if (box.checked) R.versions.push({ key, suite: v.suite, version: v.version, app });
        });
        const about = `${a.kind} ${a.version || ''} · ${a.databases} db · ${a.config ? 'config' : 'no config'}${S.services.includes(app) ? '' : ' · not selected'}`;
        rows.append(el('tr', {}, el('td', {}, box), el('td', { textContent: v.suite }), el('td', { textContent: v.version }),
          el('td', {}, el('span', { className: 'pill pill-' + String(v.status).replace(/[^\w-]/g, ''), textContent: v.status })),
          el('td', {}, app, ' ', el('span', { className: 'muted small', textContent: about })),
          el('td', { className: 'num', textContent: fmtBytes(v.total_bytes) })));
      }
    }
    if (scan.legacy.length) {
      add(el('h3', { textContent: 'App-native backup zips' }),
        el('p', { className: 'muted small', textContent: 'From a previous install. Choosing one copies its config file into appdata/<app>/ now, so the app starts with your old settings (API key, UI settings); init.sh then adds the Postgres keys. Database contents are not taken from these.' }));
      const byApp = new Map(); for (const z of scan.legacy) byApp.set(z.app, [...(byApp.get(z.app) || []), z]);
      for (const [app, zs] of byApp) {
        const has = scan.configs.includes(app);
        const pick = el('select', { disabled: has }, el('option', { value: '', textContent: '— don\'t use —' }),
          ...zs.filter(z => z.has_config).map(z => el('option', { value: z.path, textContent: `${z.name} · ${fmtBytes(z.size)}${z.has_sqlite ? ' · has sqlite db' : ''}` })));
        pick.value = R.legacy[app] || '';
        pick.addEventListener('change', () => { if (pick.value) R.legacy[app] = pick.value; else delete R.legacy[app]; });
        add(el('div', { className: 'form-row' }, el('label', { className: 'l', textContent: app }),
          el('div', {}, pick, has ? el('div', { className: 'hint', textContent: 'a config already exists for this app' }) : null)));
      }
    }
  }

  async function renderReview() {
    const p = await api('POST', '/api/setup/preview', S);
    body.innerHTML = '<h2>Review</h2>' +
      `<p class="small">Services: <b>${esc(S.services.join(', '))}</b> · VPN: <b>${esc(S.vpn.mode)}</b> · restore after install: <b>${R.versions.length}</b> version item(s)${Object.keys(R.legacy).length ? ' · legacy configs: ' + esc(Object.keys(R.legacy).join(', ')) : ''}</p>` +
      `<pre class="env">${esc(p.env)}</pre>` +
      (D.env_exists ? '<label class="inline check warn"><input type="checkbox" id="overwrite"> .env already exists — overwrite it (a .env.bak-&lt;timestamp&gt; copy is kept)</label>' : '') +
      '<p class="muted small">Writing creates .env and seeds appdata/wranglarr/wranglarr.toml if missing' +
      (R.versions.length ? ', writes the restore plan' : '') +
      (Object.keys(R.legacy).length ? ', and copies config.xml from the app backups you picked' : '') + '.</p>';
  }

  async function doWrite() {
    if (D.auth_only) { written = await api('POST', '/api/setup/write-auth', { answers: S }); return; }
    const ow = $('#overwrite'); if (D.env_exists && !(ow && ow.checked)) throw new Error('confirm overwriting .env');
    written = await api('POST', '/api/setup/write', { answers: S, restore_versions: R.versions, legacy_configs: R.legacy, overwrite: !!(ow && ow.checked) });
  }
  function renderDone() {
    body.innerHTML = '<h2>Done — next steps</h2><p class="small">Written:</p><ul class="plain small">' + written.written.map(w => `<li><code>${esc(w)}</code></li>`).join('') + '</ul>' +
      '<ol>' + written.next_steps.map(s => `<li>${esc(s)}</li>`).join('') + '</ol>' +
      '<p class="muted small">You can close this and stop the setup server (Ctrl-C).</p>';
    $('#next').hidden = true; $('#back').hidden = true;
  }

  // ---- navigation ----------------------------------------------------------------
  function paint() {
    $('#steps').innerHTML = STEPS.map((s, i) => `<span class="st ${i === cur ? 'active' : i < cur ? 'done' : ''}">${i + 1}. ${s.title}</span>`).join('');
    $('#side-steps').innerHTML = STEPS.map((s, i) => `<a class="${i === cur ? 'active' : ''}"><span class="ico">${i < cur ? '✓' : i + 1}</span>${s.title}</a>`).join('');
    $('#tb-title').textContent = `Setup · ${STEPS[cur].title}`;
    $('#back').disabled = cur === 0; $('#next').textContent = STEPS[cur].id === 'review' ? 'Write files ›' : 'Next ›';
    STEPS[cur].render();
  }
  $('#back').addEventListener('click', () => { if (cur > 0) { cur--; paint(); } });
  $('#next').addEventListener('click', async () => {
    const st = STEPS[cur];
    try {
      if (st.validate) { const err = await st.validate(); if (err) return toast(err, true); }
      if (st.id === 'review') await doWrite();
      cur++; paint();
    } catch (e) { toast(e.message, true); }
  });
  paint();
})();
