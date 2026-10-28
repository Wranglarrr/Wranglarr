// Community configs: paste → check → apply, the installed configs as a timeline per app (remove one),
// test a release name, export picked settings.
(() => {
  const { api, esc, safeUrl, toast, ask, icon } = W;
  const $ = id => document.getElementById(id);
  const when = s => s ? new Date(s).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' }) : '';
  const logo = name => `<img class="mini-logo" src="${esc(W.url(`/api/apps/${encodeURIComponent(name)}/logo`))}" alt="" onerror="this.remove()">`;
  const ACTION = { create: ['new', 'ok'], update: ['change', 'info'], same: ['same', 'muted'], keep: ['kept', 'muted'], error: ['problem', 'err'] };
  const RES = { customformat: 'custom format', qualityprofile: 'quality profile', qualitydefinition: 'quality sizes', releaseprofile: 'release profile',
                tag: 'tag', metadataprofile: 'metadata profile', delayprofile: 'delay profile', autotagging: 'auto tagging',
                'config/naming': 'file naming', 'config/mediamanagement': 'media management' };
  let O = null;                    // overview: apps, examples, history
  let I = null;                    // installed: [{app, kind, error, marks (newest first)}]
  let checked = null;              // the last check: { text, targets, choices, resolve, prefix, result }

  async function load() {
    try { O = await api('GET', '/api/configs'); } catch (e) { toast(e.message, true); return; }
    $('cc-examples').innerHTML = '<span class="muted small">Examples:</span>' + O.examples.map(x =>
      `<button class="chip cc-ex" type="button" data-ex="${esc(x.id)}" title="${esc(x.description)}">${esc(x.name)}
        <span class="muted">${x.apps.map(esc).join(', ')}</span></button>`).join('');
    const opts = O.apps.map(a => `<option value="${esc(a.name)}">${esc(a.name)}</option>`).join('');
    $('try-app').innerHTML = opts || '<option value="">no arrs</option>';
    $('ex-app').innerHTML = opts || '<option value="">no arrs</option>';
    renderHistory();
    loadInstalled();
    if (O.apps.length) inventory($('ex-app').value);
  }

  // ---- import ------------------------------------------------------------------------
  $('cc-examples').addEventListener('click', async e => {
    const b = e.target.closest('[data-ex]'); if (!b) return;
    try { $('cc-text').value = (await api('GET', `/api/configs/examples/${b.dataset.ex}`)).text; } catch (err) { toast(err.message, true); return; }
    check({});
  });
  $('cc-check').addEventListener('click', () => check({}));
  $('cc-text').addEventListener('input', () => {     // what's shown no longer matches the text: check again first
    const b = $('cc-apply');
    if (b && checked && $('cc-text').value !== checked.text) { b.disabled = true; $('cc-apply-msg').textContent = 'the text changed: check it again'; }
  });
  $('cc-clear').addEventListener('click', () => { $('cc-text').value = ''; $('cc-preview').hidden = true; $('cc-msg').textContent = ''; checked = null; });

  // opts: what changes from the last check (targets, extras choices, resolve, prefix); {} starts over
  let seq = 0;                     // only the latest check gets drawn
  async function check(opts) {
    const text = $('cc-text').value, my = ++seq;
    const prev = Object.keys(opts).length && checked ? checked : {};
    const o = { targets: prev.targets || null, choices: prev.choices || null, resolve: prev.resolve || {}, prefix: prev.prefix || null, ...opts };
    const msg = $('cc-msg');
    msg.className = 'small muted'; msg.textContent = 'checking against the apps…';
    try {
      const r = await api('POST', '/api/configs/check', { text, targets: o.targets, extras: o.choices, resolve: o.resolve, prefix: o.prefix });
      if (my !== seq) return;
      checked = { text, targets: r.chosen, result: r, choices: Object.fromEntries(r.extras.map(x => [x.key, x.on])),
                  resolve: o.resolve, prefix: r.prefix };
      msg.textContent = '';
      renderPreview(r);
    } catch (e) {
      if (my !== seq) return;
      msg.className = 'small error'; msg.textContent = e.message;
      $('cc-preview').hidden = true; checked = null;
    }
  }

  const choiceText = (x, c, r) => c === 'change' ? (x.part === 'tags' ? 'use yours' : 'change yours')
    : c === 'keep' ? 'keep yours, skip this'
    : x.part === 'tags' ? `use a new tag “${r.tag_prefix}${x.orig}”` : `add a copy, “${r.prefix}${x.orig}”`;

  const WIDE = { qualityDefinitions: 'Quality sizes', naming: 'File naming', mediaManagement: 'Media management' };
  // a config's source: its name, linked to its URL (http(s) only), or the URL alone
  const sourceText = src => {
    if (!src) return '';
    const u = safeUrl(src.url || ''), label = esc(src.name || src.url || '');
    return u ? `<a href="${esc(u)}" target="_blank" rel="noopener noreferrer">${label}</a>` : label;
  };
  function renderPreview(r) {
    const c = r.config, box = $('cc-preview');
    const kinds = Object.keys(c.apps), link = safeUrl(c.link);      // a pasted config's link: http(s) only
    const head = `<div class="cc-head"><h3 style="margin-top:0">${esc(c.name)}</h3>
      ${c.author || c.link || c.source ? `<div class="small muted">${[c.author && 'by ' + esc(c.author), c.source && 'from ' + sourceText(c.source),
        link ? `<a href="${esc(link)}" target="_blank" rel="noopener noreferrer">${esc(c.link)}</a>` : c.link ? esc(c.link) : ''].filter(Boolean).join(' · ')}</div>` : ''}
      ${c.description ? `<p class="small">${esc(c.description)}</p>` : ''}</div>`;
    const pick = kinds.map(k => {
      const fits = r.fits[k] || [];
      const label = k === '*' ? 'Custom formats for' : `The ${esc(k)} part goes to`;
      return `<div class="form-row"><label class="l">${label}</label><div class="row">${fits.length ? fits.map(n =>
        `<label class="check-line"><input type="checkbox" data-target="${esc(k)}" value="${esc(n)}" ${r.chosen[k].includes(n) ? 'checked' : ''}>
          ${logo(n)} ${esc(n)}</label>`).join('') : `<span class="muted small">no ${esc(k)} in this stack</span>`}</div></div>`;
    }).join('');
    const extras = r.extras.length ? `<div class="form-row"><label class="l">Optional extras
        <div class="hint" style="font-weight:400">the config's author made these optional: pick what you want</div></label><div class="cc-extras">${
      r.extras.map(x => `<label class="check-line"><input type="checkbox" data-extra="${esc(x.key)}" ${x.on ? 'checked' : ''}>
        <span><b>${esc(x.name)}</b> <span class="muted small">${esc(x.label)}${kinds.length > 1 ? ' · ' + esc(x.kind) : ''}</span>
        ${x.note ? `<div class="small muted">${esc(x.note)}</div>` : ''}</span></label>`).join('')}</div></div>` : '';
    const choosy = r.plans.flatMap(p => fold(p.rows).filter(x => x.can && x.can.length));   // as the lines show
    const nCan = c => choosy.filter(x => x.can.includes(c)).length;
    const nCopy = nCan('prefix'), noCopy = choosy.filter(x => !x.can.includes('prefix'));
    const wide = [...new Set(noCopy.filter(x => x.part !== 'delayProfiles').map(x => WIDE[x.part] || x.label))];
    const wideNote = [wide.length ? `${wide.join(', ')}: one set per app, so ${wide.length > 1 ? 'these' : 'they'} can be changed or kept, not copied.` : '',
      noCopy.some(x => x.part === 'delayProfiles') ? 'Delay profiles are matched by their tags: change or keep yours.' : ''].filter(Boolean).join(' ');
    const btn = (c, txt) => nCan(c) ? `<button class="btn small" type="button" data-all="${c}">${txt} <span class="muted">(${nCan(c)})</span></button>` : '';
    const resolveBar = choosy.length ? `<div class="form-row"><label class="l">Settings you already have
        <div class="hint" style="font-weight:400">${choosy.length} line${choosy.length > 1 ? 's' : ''} below touch${choosy.length > 1 ? '' : 'es'} something of yours</div></label>
      <div><div class="row cc-resolve">${btn('change', 'Change mine')}${btn('keep', 'Keep mine')}${nCopy ? `${btn('prefix', 'Add copies named')}
        <input id="cc-prefix" value="${esc(r.prefix)}" maxlength="40" spellcheck="false" style="width:90px" title="the copies' names start with this">
        <span class="muted small">+ the name</span>` : ''}</div>
        <div class="hint">Or pick per line.${nCopy ? ` A copy leaves yours as it is, and the rest of the config uses the copy:
          its profiles score the copied custom formats, its release and delay profiles get the new tag.` : ''}${wideNote ? ' ' + esc(wideNote) : ''}</div></div></div>` : '';
    const plans = r.plans.map(p => {
      if (p.error) return `<h3>${logo(p.app)} ${esc(p.app)}</h3><p class="error small">${esc(p.error)}</p>`;
      const rows = fold(p.rows).map(x => {
        const [txt, cls] = ACTION[x.action];
        const sel = x.can && x.can.length ? `<select data-resolve="${esc(x.key)}" title="this is already in ${esc(p.app)}">${
          x.can.map(c => `<option value="${esc(c)}" ${x.choice === c ? 'selected' : ''}>${esc(choiceText(x, c, r))}</option>`).join('')}</select>` : '';
        return `<tr class="cc-${esc(x.action)}"><td><span class="pill pill-c-${cls}">${txt}</span></td>
          <td class="muted small">${esc(x.label)}</td><td><b>${esc(x.name)}</b>
          ${x.details.length ? `<div class="small ${x.action === 'error' ? 'error' : 'muted'}">${x.details.map(esc).join(x.lines ? '<br>' : ' · ')}</div>` : ''}
          ${x.warnings.map(w => `<div class="small warn-text">${icon('warning')} ${esc(w)}</div>`).join('')}</td>
          <td class="cc-choice">${sel}</td></tr>`;
      }).join('');
      const sum = p.errors ? `<span class="error">${esc(p.errors)} problem${p.errors > 1 ? 's' : ''}</span>`
        : p.changes ? `${esc(p.changes)} change${p.changes > 1 ? 's' : ''}` : 'already set up like this';
      const warn = p.warnings && !p.errors ? ` · <span class="warn-text">${esc(p.warnings)} to look at</span>` : '';
      return `<h3>${logo(p.app)} ${esc(p.app)} <span class="muted small" style="text-transform:none;letter-spacing:0">· ${sum}${warn}</span></h3>
        <table class="compact cc-plan"><tbody>${rows}</tbody></table>`;
    }).join('');
    const changes = r.plans.reduce((n, p) => n + (p.error || p.errors ? 0 : p.changes), 0);
    const blocked = r.plans.some(p => p.error || p.errors);
    box.innerHTML = head + extras + pick + resolveBar + (plans || '<p class="muted small">Pick an app to see what would change.</p>') +
      `<div class="row" style="margin-top:12px">
        <button class="btn primary" id="cc-apply" ${changes && !blocked ? '' : 'disabled'}>${changes ? `Apply ${esc(changes)} change${changes > 1 ? 's' : ''}` : 'Nothing to apply'}</button>
        ${blocked ? '<span class="small error">Fix the problems first: nothing is applied while one is listed.</span>' : ''}
        <span class="small" id="cc-apply-msg"></span></div>`;
    box.hidden = false;
  }

  // quality sizes come one per quality: show them as one row per action
  function fold(rows) {
    const out = [], seen = {};
    for (const x of rows) {
      if (x.part !== 'qualityDefinitions' || x.action === 'error' || x.action === 'keep') { out.push(x); continue; }
      let g = seen[x.action];
      if (!g) { g = seen[x.action] = { ...x, names: [], details: [], warnings: [...x.warnings], lines: true }; out.push(g); }
      g.names.push(x.name);
      if (x.details.length) g.details.push(`${x.name}: ${x.details.join(', ')}`);
    }
    for (const g of Object.values(seen)) g.name = g.names.length === 1 ? g.names[0] : `${g.names.length} qualities`;
    return out;
  }

  $('cc-preview').addEventListener('change', e => {
    const t = e.target;
    if (!checked) return;
    if (t.dataset.extra) return check({ choices: { ...checked.choices, [t.dataset.extra]: t.checked } });
    if (t.dataset.resolve) return check({ resolve: { ...checked.resolve, [t.dataset.resolve]: t.value } });
    if (!t.dataset.target) return;
    const targets = {};
    for (const k of Object.keys(checked.result.config.apps))
      targets[k] = [...$('cc-preview').querySelectorAll(`input[data-target="${CSS.escape(k)}"]:checked`)].map(i => i.value);
    check({ targets });
  });

  $('cc-preview').addEventListener('keydown', e => {
    if (e.target.id === 'cc-prefix' && e.key === 'Enter') { e.preventDefault(); $('cc-preview').querySelector('[data-all=prefix]').click(); }
  });
  $('cc-preview').addEventListener('click', async e => {
    const all = e.target.closest('[data-all]');
    if (all && checked) {
      const v = all.dataset.all, resolve = { ...checked.resolve };
      for (const p of checked.result.plans) for (const x of p.rows) if (x.can && x.can.includes(v)) resolve[x.key] = v;
      const prefix = $('cc-prefix') ? $('cc-prefix').value : checked.prefix;
      return check({ resolve, prefix });
    }
    if (e.target.id !== 'cc-apply' || !checked) return;
    const apps = checked.result.plans.filter(p => p.changes).map(p => p.app);
    const prot = checked.result.plans.filter(p => p.protected && p.protected.length);
    const ok = prot.length ? await ackDialog(checked.result.config.name, prot)
      : await ask({ title: `Apply "${checked.result.config.name}"?`, ok: 'Apply',
          body: `Changes ${apps.join(', ')} now. It's listed under Installed, where you can remove it again.` });
    if (!ok) return;
    const btn = e.target; btn.disabled = true;
    try {
      const r = await api('POST', '/api/configs/apply', { text: checked.text, targets: checked.targets, extras: checked.choices,
                                                           resolve: checked.resolve, prefix: checked.prefix, ack: !!prot.length });
      O.history = r.history; renderHistory(); loadInstalled();
      const bad = r.results.filter(x => x.error);
      if (bad.length) toast(bad.map(x => `${x.app}: ${x.error}`).join(' · '), true);
      else toast(`Applied to ${r.results.map(x => x.app).join(', ')}`);
      await check({ targets: checked.targets });
    } catch (err) { toast(err.message, true); btn.disabled = false; }
  });
  // ---- dialogs with lists in them ----------------------------------------------------------
  // buttons: [{label, value, cls}]; resolves to the value picked (a checked radio's, when there is one), or null
  function dialog({ title, html, buttons }) {
    return new Promise(resolve => {
      const d = document.createElement('dialog');
      d.className = 'dlg modal cc-dlg';
      d.setAttribute('aria-labelledby', 'modal-title');
      d.innerHTML = `<form method="dialog"><h3 id="modal-title">${esc(title)}</h3><div class="modal-body">${html}</div>
        <div class="modal-actions"><button class="btn ghost" type="button" data-cancel>Cancel</button>
        ${buttons.map(b => `<button class="btn ${b.cls || 'primary'}" value="${esc(b.value)}">${esc(b.label)}</button>`).join('')}</div></form>`;
      let picked = null;
      d.querySelector('[data-cancel]').addEventListener('click', () => d.close('cancel'));
      d.addEventListener('click', e => { if (e.target === d) d.close('cancel'); });
      d.querySelector('form').addEventListener('submit', e => {
        const r = d.querySelector('input[type=radio]:checked');
        picked = r ? r.value : (e.submitter && e.submitter.value);
      });
      d.addEventListener('close', () => {
        const yes = !!d.returnValue && d.returnValue !== 'cancel';
        d.remove();
        resolve(yes ? picked : null);
      });
      document.body.append(d);
      d.showModal();
      d.querySelector('[data-cancel]').focus();
    });
  }

  // [{part, label, name, changes: [line]}] as a short list
  const changeList = (items, limit = 12) => {
    if (!items || !items.length) return '<p class="muted small">Nothing in the app changes.</p>';
    const li = items.slice(0, limit).map(i => `<li><span class="muted">${esc(i.label)}</span> <b>${esc(i.name)}</b>
      ${i.changes.length ? `<span class="small">${i.changes.map(esc).join(' · ')}</span>` : ''}</li>`).join('');
    return `<ul class="cc-changes">${li}${items.length > limit ? `<li class="muted">and ${items.length - limit} more</li>` : ''}</ul>`;
  };
  // what a removal doesn't check yet: the films, series or artists that use what it changes
  const LIBRARY = { radarr: 'films', sonarr: 'series', lidarr: 'artists', whisparr: 'titles' };
  function blastRadius(app, ...ways) {
    const a = (I || []).find(x => x.app === app), lib = (a && LIBRARY[a.kind]) || 'items';
    const gone = [...new Set(ways.flatMap(w => w.effects)
      .filter(i => (i.part === 'qualityProfiles' || i.part === 'metadataProfiles') && i.changes.includes('removed')).map(i => i.name))];
    return `<div class="cc-blast"><b>${icon('warning')} Not checked: which ${esc(lib)} this affects</b>
      <p class="small">wranglarr doesn't look yet at which ${esc(lib)} in ${esc(app)} use what this changes. Changed settings apply to every one of
      them${gone.length ? `, and some may still be set to ${gone.map(n => `<b>${esc(n)}</b>`).join(', ')}, which ${gone.length === 1 ? 'is' : 'are'} removed here.
      ${esc(app)} may refuse to delete a profile in use: it then stays, with a note` : ''}. To be sure, check ${esc(app)}'s
      ${esc(lib)} first (the library's editor can filter by profile).</p></div>`;
  }
  // the day in local time, as the removal notes write it (YYYY-MM-DD)
  const day = at => { const d = new Date(at), p = n => String(n).padStart(2, '0'); return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`; };
  const markName = m => m.kind === 'external' ? `${m.label} ${day(m.at)}` : m.label;

  // the OK for what a config changes that's built in, app-wide, or another config's
  function ackDialog(name, plans) {
    const rows = plans.map(p => `<p class="small" style="margin-bottom:2px">${logo(p.app)} <b>${esc(p.app)}</b></p><ul class="cc-changes">${p.protected.map(x =>
      `<li>${WIDE[x.part] ? '' : `<span class="muted">${esc(x.label)}</span> `}<b>${esc(x.name)}</b>${x.items && x.items.length
        ? ` <span class="small">${x.items.slice(0, 8).map(esc).join(', ')}${x.items.length > 8 ? '…' : ''}</span>` : ''}
        ${x.why ? `<div class="small muted">${esc(x.why)}</div>` : ''}
        ${x.overrides.length ? `<div class="small warn-text">changes what ${x.overrides.map(n => `"${esc(n)}"`).join(', ')} set</div>` : ''}</li>`).join('')}</ul>`).join('');
    return dialog({ title: `"${name}" changes shared settings`, buttons: [{ label: 'Change them', value: 'ok', cls: 'danger-fill' }],
      html: `<p>These reach further than the config's own new items. Removing it later puts them back.</p>${rows}` });
  }

  // ---- installed: each app's timeline -----------------------------------------------------
  async function loadInstalled() {
    try { I = (await api('GET', '/api/configs/installed')).installed; }
    catch (e) { $('cc-installed').innerHTML = `<p class="warn-text small">${esc(e.message)}</p>`; return; }
    renderInstalled();
  }

  const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;

  function renderInstalled() {
    if (!I.length) { $('cc-installed').innerHTML = '<p class="muted small" style="margin:0">Nothing installed yet.</p>'; return; }
    $('cc-installed').innerHTML = I.map(a => `<div class="cc-tl-app"><h3>${logo(a.app)} ${esc(a.app)}</h3>
      ${a.error ? `<p class="small warn-text">Couldn't look for changes made in the app: ${esc(a.error)}</p>` : ''}
      <ol class="cc-tl">${a.marks.map(m => {
        const n = m.items.reduce((t, i) => t + i.changes.length, 0);
        const state = [m.partial && '<span class="pill pill-partial" title="the install stopped part way">partly</span>',
                       m.kept.length && `<span class="pill" title="${esc(m.kept.join('\n'))}">partly removed</span>`].filter(Boolean).join(' ');
        const what = m.kind === 'before' ? `what ${plural(m.items.length, 'item')} had before wranglarr first changed them`
          : `${plural(m.items.length, 'item')}, ${plural(n, 'change')}`;
        return `<li class="cc-tl-${esc(m.kind)}"><div class="cc-tl-head"><span class="cc-tl-dot"></span>
          <b>${esc(markName(m))}</b>${m.source ? ` <span class="small muted">from ${sourceText(m.source)}</span>` : ''} ${state} <span class="small muted">${m.kind === 'external' ? 'noticed' : ''} ${esc(when(m.at))}</span>
          ${m.kind === 'config' ? `<button class="btn small" data-remove="${esc(m.id)}" data-app="${esc(a.app)}">Remove</button>` : ''}</div>
          <details><summary class="small muted">${esc(what)}</summary>${changeList(m.items, 200)}
            ${m.kept.length ? `<p class="small muted">${m.kept.map(esc).join('<br>')}</p>` : ''}</details></li>`;
      }).join('')}</ol></div>`).join('');
  }

  $('cc-installed').addEventListener('click', async e => {
    const b = e.target.closest('[data-remove]'); if (!b) return;
    const app = b.dataset.app, id = b.dataset.remove, at = `/api/configs/installed/${encodeURIComponent(app)}/${encodeURIComponent(id)}`;
    b.disabled = true;
    try {
      const pv = await api('GET', at);
      let throughout = false;
      if (pv.mode === 'confirm') {
        const ok = await dialog({ title: `Remove "${pv.label}" from ${app}?`, buttons: [{ label: 'Remove', value: 'ok', cls: 'danger-fill' }],
          html: `<p>This puts back what it changed:</p>${changeList(pv.keep.effects)}${blastRadius(app, pv.keep)}` });
        if (!ok) return;
      } else if (pv.mode === 'choose') {
        const later = pv.throughout.overrides.map(o => `<li><b>${esc(markName(o))}</b>${changeList(o.items, 6)}</li>`).join('');
        const kept = w => w.kept.length ? `<p class="small muted">${w.kept.map(esc).join('<br>')}</p>` : '';
        const v = await dialog({ title: `Remove "${pv.label}" from ${app}?`, buttons: [{ label: 'Remove', value: 'ok', cls: 'danger-fill' }],
          html: `<p>Changes made after it touch the same settings, or use what it made. Pick what happens to them.</p>
            <label class="cc-way"><input type="radio" name="way" value="keep" checked><span><b>Keep dependent changes as they are</b>
              <span class="small muted">Later changes stay, and what they use stays with them. In the app:</span>
              ${changeList(pv.keep.effects)}${kept(pv.keep)}</span></label>
            <label class="cc-way"><input type="radio" name="way" value="throughout"><span><b>Remove its changes throughout</b>
              <span class="small muted">Back to how things were before it, over these later changes too:</span>
              ${later ? `<ul class="cc-changes">${later}</ul>` : ''}<span class="small muted">In the app:</span>
              ${changeList(pv.throughout.effects)}${kept(pv.throughout)}</span></label>
            ${blastRadius(app, pv.keep, pv.throughout)}` });
        if (!v) return;
        throughout = v === 'throughout';
      }
      const r = await api('POST', `${at}/remove`, { throughout });
      I = r.installed; renderInstalled();
      toast(r.notes.length ? r.notes.join(' · ')
        : pv.mode === 'none' ? `Removed "${pv.label}": nothing in ${app} changes` : `Removed "${pv.label}" from ${app}`, !!r.notes.length);
      if (checked) check({ targets: checked.targets });
    } catch (err) { toast(err.message, true); }
    finally { b.disabled = false; }
  });

  // ---- the change log: every import with the copies it replaced, and its undo ------------------
  function renderHistory() {
    const h = O.history;
    $('cc-log').hidden = !h.length;
    if (!h.length) { $('cc-history').innerHTML = ''; return; }

    $('cc-history').innerHTML = `<table class="compact cc-hist"><thead><tr><th>When</th><th>Config</th><th>App</th><th>What</th><th></th></tr></thead><tbody>${h.map(x => {
      const made = x.items.filter(i => i.created).length;
      const changed = x.items.filter(i => !i.created && i.res !== 'qualitydefinition').length;
      const sizes = x.items.filter(i => i.res === 'qualitydefinition').map(i => i.name);
      const what = [made && `${made} added`, changed && `${changed} changed`, ...sizes].filter(Boolean).join(', ');
      const state = x.undone ? `<span class="pill">undone</span>` : x.partial ? `<span class="pill pill-partial" title="stopped part way">partly</span>` : '';
      return `<tr class="${x.undone ? 'off' : ''}"><td class="small">${esc(when(x.at))}</td><td><b>${esc(x.config)}</b> ${state}
        ${x.undo_notes && x.undo_notes.length ? `<div class="small muted">${x.undo_notes.map(esc).join(' · ')}</div>` : ''}</td>
        <td>${logo(x.app)} ${esc(x.app)}</td><td class="small" title="${esc(x.items.map(i => `${RES[i.res] || i.res}: ${i.name}${i.created ? ' (new)' : ''}`).join('\n'))}">${esc(what)}</td>
        <td class="num">${x.undone ? '' : `<button class="btn small" data-undo="${esc(x.id)}">Undo</button>`}
          <button class="btn small ghost" data-forget="${esc(x.id)}" title="remove from this list (doesn't change the app)">Forget</button></td></tr>`;
    }).join('')}</tbody></table>`;
  }

  $('cc-history').addEventListener('click', async e => {
    const u = e.target.closest('[data-undo]'), f = e.target.closest('[data-forget]');
    if (u) {
      const x = O.history.find(h => h.id === u.dataset.undo);
      const ok = await ask({ title: `Undo "${x.config}" on ${x.app}?`, ok: 'Undo', danger: true,
        body: 'What it added is deleted and what it changed is put back from the copy saved when it was imported. Edits made since then to those same settings are lost: Remove under Installed keeps them.' });
      if (!ok) return;
      try {
        const r = await api('POST', `/api/configs/history/${x.id}/undo`);
        O.history = r.history; renderHistory(); loadInstalled();
        toast(r.notes.length ? r.notes.join(' · ') : `Undone on ${x.app}`, !!r.notes.length);
        if (checked) check({ targets: checked.targets });
      } catch (err) { toast(err.message, true); }
    } else if (f) {
      try { O.history = (await api('DELETE', `/api/configs/history/${f.dataset.forget}`)).history; renderHistory(); }
      catch (err) { toast(err.message, true); }
    }
  });

  // ---- test a release name ---------------------------------------------------------
  $('try-form').addEventListener('submit', async e => {
    e.preventDefault();
    const f = e.target, out = $('try-out');
    if (!f.title.value.trim()) return;
    out.innerHTML = '<span class="muted">asking…</span>';
    try {
      const r = await api('POST', '/api/configs/try', { app: f.app.value, title: f.title.value });
      const fmts = r.formats.length ? r.formats.map(n => `<span class="chip">${esc(n)}</span>`).join('') : '<span class="muted">none</span>';
      const rows = r.profiles.map(p => {
        const verdict = !p.allowed ? '<span class="muted">quality not in this profile</span>'
          : p.score < p.min ? `<span class="error">skipped: needs ${esc(p.min)}</span>` : '<span class="ok-text">would take it</span>';
        return `<tr><td>${esc(p.name)}</td><td class="num">${esc(p.score)}</td><td>${verdict}</td></tr>`;
      }).join('');
      out.innerHTML = `<div>Quality <b>${esc(r.quality || 'unknown')}</b>${r.languages.length ? ` · language ${r.languages.map(esc).join(', ')}` : ''}</div>
        <div style="margin:6px 0">Custom formats: ${fmts}</div>
        <table class="compact"><thead><tr><th>Profile</th><th class="num">Score</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
    } catch (err) { out.innerHTML = `<span class="error">${esc(err.message)}</span>`; }
  });

  // ---- export ------------------------------------------------------------------------
  const PARTS = [['customFormats', 'Custom formats'], ['qualityProfiles', 'Quality profiles'], ['metadataProfiles', 'Metadata profiles'],
                 ['qualityDefinitions', 'Quality sizes'], ['releaseProfiles', 'Release profiles'], ['delayProfiles', 'Delay profiles'],
                 ['autoTags', 'Auto tagging'], ['tags', 'Tags'], ['naming', 'File naming'], ['mediaManagement', 'Media management']];
  const HINT = {
    customFormats: 'ticking a profile ticks the custom formats it scores; untick any you leave out (its score goes too)',
    qualityDefinitions: 'How big a file of each quality may be for its length. The arr turns down a release under the minimum or over the maximum, and of the rest leans toward the preferred size. App-wide.',
    delayProfiles: 'matched by their tags · the untagged one is the app\'s default',
    autoTags: 'conditions on a root folder are left out: that\'s a path on this server',
    tags: 'tags on what you pick come along by themselves; tick one here to make it on its own',
    naming: 'app-wide', mediaManagement: 'app-wide · paths, permissions and scripts never go in a config' };
  // each part's page in the arr's own settings (same routes in Radarr, Sonarr, Lidarr, Readarr)
  const PAGE = { customFormats: 'settings/customformats', qualityProfiles: 'settings/profiles', metadataProfiles: 'settings/profiles',
                 qualityDefinitions: 'settings/quality', releaseProfiles: 'settings/profiles', delayProfiles: 'settings/profiles',
                 autoTags: 'settings/tags', tags: 'settings/tags', naming: 'settings/mediamanagement', mediaManagement: 'settings/mediamanagement' };
  const appPage = k => { const u = W.safeUrl(inv.url || ''); return u && PAGE[k] ? u.replace(/\/+$/, '') + '/' + PAGE[k] : ''; };
  // quality sizes, with what that comes to for an hour: MiB per minute of runtime in Radarr, Sonarr and
  // Whisparr; an average bitrate in kbit/s in Lidarr (its UI shows 128 × the value in bytes a second)
  const SIZE_UNIT = { lidarr: ['kbit/s (average bitrate)', v => v * 128 * 3600 / 1048576] };
  const sizeUnit = () => SIZE_UNIT[inv.kind] || ['MB per minute of runtime', v => v * 60];
  const perHour = v => { const mb = sizeUnit()[1](v); return mb >= 1024 ? `${(mb / 1024).toFixed(mb >= 10240 ? 0 : 1)} GB` : `${Math.round(mb)} MB`; };
  const size = v => v == null || v === '' ? '<span class="muted">no limit</span>'
    : `${esc(+v)}<span class="cc-size-h">${+v ? `≈ ${perHour(+v)} an hour` : '&nbsp;'}</span>`;
  const NAMED = ['customFormats', 'qualityProfiles', 'metadataProfiles', 'releaseProfiles', 'autoTags'];
  const PART = { customFormats: 'custom format', qualityProfiles: 'quality profile', metadataProfiles: 'metadata profile',
                 releaseProfiles: 'release profile', autoTags: 'auto tagging', tags: 'tag' };
  const picks = {};                // app → part → Set(names)
  const auto = {};                 // app → Set(custom formats ticked because a profile scores them)
  const invs = {};                 // app → its inventory
  const marks = {};                // "app|part|name" → { mode: 'always' | 'on' | 'off', note }
  const renames = {};              // "app|part|name" → the name it gets in the config
  let inv = null;
  $('ex-app').addEventListener('change', e => inventory(e.target.value));
  // back from the arr's own settings (a link above): read it again, so what was made there shows up
  let away = false;
  $('ex-lists').addEventListener('click', e => { if (e.target.closest('a[target=_blank]')) away = true; });
  document.addEventListener('visibilitychange', () => { if (!document.hidden && away && inv) { away = false; inventory(inv.app); } });

  async function inventory(app) {
    if (!app) return;
    $('ex-lists').innerHTML = '<p class="muted small">reading…</p>';
    try { inv = invs[app] = await api('GET', `/api/configs/inventory/${encodeURIComponent(app)}`); }
    catch (e) { $('ex-lists').innerHTML = `<p class="error small">${esc(e.message)}</p>`; return; }
    picks[app] = picks[app] || Object.fromEntries(PARTS.map(([k]) => [k, new Set()]));
    auto[app] = auto[app] || new Set();
    renderLists();
  }

  // custom format → the picked profiles that score it
  const scoredBy = app => {
    const m = {};
    for (const p of (invs[app] || {}).qualityProfiles || []) if (picks[app].qualityProfiles.has(p.name)) p.formats.forEach(f => (m[f] = m[f] || []).push(p.name));
    return m;
  };

  function renderLists() {
    const app = inv.app, by = scoredBy(app);
    $('ex-lists').innerHTML = PARTS.map(([k, label]) => {
      if (inv[k] == null) return '';               // the app has no such part (or didn't give it)
      const rows = inv[k];
      const chips = x => (x.builtin ? ` <span class="chip cc-chip" title="${esc(inv.kind)} ships with this one">built-in</span>` : '')
        + (x.appWide ? ' <span class="chip cc-chip">default</span>' : '')
        + (k === 'customFormats' && by[x.name] ? ` <span class="chip cc-chip" title="scored by ${esc(by[x.name].join(', '))}">scored</span>` : '');
      const box = x => `<input type="checkbox" data-part="${k}" value="${esc(x.name)}" ${picks[app][k].has(x.name) ? 'checked' : ''}>`;
      const page = appPage(k);
      const open = page ? ` <a class="cc-open" href="${esc(page)}" target="_blank" rel="noopener" title="${esc(`Open ${label.toLowerCase()} in ${app}`)}"
        aria-label="${esc(`Open ${label.toLowerCase()} in ${app} (new tab)`)}">${icon('external')}</a>` : '';
      const list = !rows.length
        ? `<p class="muted small cc-none">None in ${esc(app)} yet${page ? `: <a href="${esc(page)}" target="_blank" rel="noopener">add some there</a>` : ''}.</p>`
        : k === 'qualityDefinitions'
        ? `<div class="cc-sizes-wrap"><table class="compact cc-sizes"><thead><tr><th></th><th>Quality</th><th class="num">Minimum</th><th class="num">Preferred</th>
            <th class="num">Maximum</th></tr><tr class="cc-unit"><th></th><th></th><th class="num" colspan="3">${esc(sizeUnit()[0])}</th></tr></thead>
          <tbody>${rows.map(x => `<tr><td>${box(x)}</td><td>${esc(x.name)}</td><td class="num">${size(x.min)}</td><td class="num">${size(x.preferred)}</td>
            <td class="num">${size(x.max)}</td></tr>`).join('')}</tbody></table></div>`
        : `<div class="cc-picks">${rows.map(x => `<label class="check-line" title="${esc(x.detail)}">
          ${box(x)} <span class="cc-pn">${esc(x.name)}${chips(x)}</span>
          <span class="muted small">${esc(x.detail)}</span></label>`).join('')}</div>`;
      return `<div class="form-row"><label class="l">${label}${open}${inv.appWide.includes(k) ? '<div class="hint" style="font-weight:400">app-wide</div>' : ''}</label><div>
        ${list}
        ${HINT[k] ? `<div class="hint">${HINT[k]}</div>` : ''}
        ${rows.length > 6 ? `<div class="hint"><button class="linkish" type="button" data-all="${k}">all</button> · <button class="linkish" type="button" data-none="${k}">none</button></div>` : ''}
        </div></div>`;
    }).join('') || '<p class="muted small">This app has nothing to export.</p>';
    count();
  }

  function tick(app, k, name, on) {
    const s = picks[app][k];
    on ? s.add(name) : s.delete(name);
    if (k === 'customFormats') auto[app].delete(name);
    if (k !== 'qualityProfiles') return;
    const p = (invs[app].qualityProfiles || []).find(x => x.name === name);
    if (!p) return;
    if (on) { for (const f of p.formats) if (!picks[app].customFormats.has(f)) { picks[app].customFormats.add(f); auto[app].add(f); } }
    else {
      const still = scoredBy(app);
      for (const f of p.formats) if (auto[app].has(f) && !still[f]) { picks[app].customFormats.delete(f); auto[app].delete(f); }
    }
  }

  $('ex-lists').addEventListener('change', e => {
    const k = e.target.dataset.part; if (!k) return;
    tick(inv.app, k, e.target.value, e.target.checked);
    renderLists();
  });
  $('ex-lists').addEventListener('click', e => {
    const k = e.target.dataset.all || e.target.dataset.none; if (!k) return;
    for (const x of inv[k] || []) tick(inv.app, k, x.name, !!e.target.dataset.all);
    renderLists();
  });

  function count() {
    const per = Object.entries(picks).map(([a, p]) => [a, PARTS.reduce((n, [k]) => n + p[k].size, 0)]).filter(([, n]) => n);
    $('ex-count').textContent = per.length ? 'picked: ' + per.map(([a, n]) => `${a} ${n}`).join(', ') : '';
    renderMarks();
  }

  // everything with a name that will be in the config: rename it, make it an optional extra
  function exported() {
    const out = [];
    for (const [app, p] of Object.entries(picks)) for (const k of [...NAMED, 'tags']) for (const n of p[k]) out.push([app, k, n]);
    return out;
  }
  const slug = s => (s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'new') + '-';
  const builtinOf = (app, part, name) => ((invs[app] || {})[part] || []).some(x => x.name === name && x.builtin);

  function renderMarks() {
    const rows = exported();
    $('ex-extras-row').hidden = !rows.length;
    const multi = new Set(rows.map(r => r[0])).size > 1;
    $('ex-extras').innerHTML = rows.map(([app, part, name]) => {
      const k = `${app}|${part}|${name}`, m = marks[k] || { mode: 'always', note: '' }, b = builtinOf(app, part, name);
      const warn = b && !renames[k] ? `<div class="small warn-text">${icon('warning')} ${esc(invs[app].kind)}'s own ${PART[part]}: importing it changes theirs unless they take a copy. Give it a name of its own.</div>` : '';
      return `<tr><td><b>${esc(name)}</b> <span class="muted small">${PART[part]}${multi ? ' · ' + esc(app) : ''}</span>${warn}</td>
        <td><input data-rename="${esc(k)}" value="${esc(renames[k] || '')}" placeholder="${esc(name)}" maxlength="${part === 'tags' ? 60 : 100}" spellcheck="false"></td>
        <td>${part === 'tags' ? '' : `<select data-mark="${esc(k)}" style="width:auto">
          <option value="always" ${m.mode === 'always' ? 'selected' : ''}>always</option>
          <option value="on" ${m.mode === 'on' ? 'selected' : ''}>extra, on unless switched off</option>
          <option value="off" ${m.mode === 'off' ? 'selected' : ''}>extra, off unless switched on</option></select>`}</td>
        <td>${m.mode === 'always' ? '' : `<input data-note="${esc(k)}" value="${esc(m.note)}" maxlength="300" placeholder="what it does" style="width:100%">`}</td></tr>`;
    }).join('');
    const wide = [];
    for (const [app, p] of Object.entries(picks)) {
      for (const k of (invs[app] || {}).appWide || []) if (p[k].size) wide.push(PARTS.find(x => x[0] === k)[1].toLowerCase());
      if (p.delayProfiles.has('default')) wide.push('the default delay profile');
    }
    $('ex-appwide').innerHTML = wide.length ? `${icon('warning')} ${esc([...new Set(wide)].join(', '))}: these change the whole app that imports them.
      Whoever imports can keep theirs instead.` : '';
  }
  $('ex-extras').addEventListener('change', e => {
    const k = e.target.dataset.mark; if (!k) return;
    marks[k] = { note: '', ...marks[k], mode: e.target.value };
    renderMarks();
  });
  $('ex-extras').addEventListener('input', e => {
    if (e.target.dataset.note) marks[e.target.dataset.note] = { ...marks[e.target.dataset.note], note: e.target.value };
    if (e.target.dataset.rename) renames[e.target.dataset.rename] = e.target.value.trim();
  });
  $('ex-extras').addEventListener('change', e => { if (e.target.dataset.rename) renderMarks(); });
  const prefixAll = onlyBuiltin => {
    const pre = $('ex-prefix').value || $('ex-prefix').placeholder;
    for (const [app, part, name] of exported()) {
      if (onlyBuiltin && !builtinOf(app, part, name)) continue;
      renames[`${app}|${part}|${name}`] = part === 'tags' ? slug(pre) + name : pre + name;
    }
    renderMarks();
  };
  $('ex-prefix-builtin').addEventListener('click', () => prefixAll(true));
  $('ex-prefix-all').addEventListener('click', () => prefixAll(false));
  $('ex-prefix-clear').addEventListener('click', () => { for (const k of Object.keys(renames)) delete renames[k]; renderMarks(); });

  $('ex-make').addEventListener('click', async () => {
    const apps = {};
    for (const [a, p] of Object.entries(picks)) {
      if (!PARTS.some(([k]) => p[k].size)) continue;
      const want = Object.fromEntries(PARTS.map(([k]) => [k, [...p[k]]]));
      const extras = {}, ren = {};
      for (const [k, m] of Object.entries(marks)) {
        const [app, part, name] = k.split('|');
        if (app !== a || m.mode === 'always' || !p[part] || !p[part].has(name)) continue;
        (extras[part] = extras[part] || {})[name] = { default: m.mode === 'on', note: m.note || '' };
      }
      for (const [k, v] of Object.entries(renames)) {
        const [app, part, name] = k.split('|');
        if (app !== a || !v || v === name || !p[part] || !p[part].has(name)) continue;
        (ren[part] = ren[part] || {})[name] = v;
      }
      apps[a] = { ...want, extras, renames: ren };
    }
    const msg = $('ex-msg'); msg.className = 'small'; msg.textContent = '';
    $('ex-notes').innerHTML = '';
    try {
      const r = await api('POST', '/api/configs/export', { name: $('ex-name').value.trim(), description: $('ex-desc').value.trim(),
                                                            author: $('ex-author').value.trim(), apps,
                                                            source: { name: $('ex-source-name').value.trim(), url: $('ex-source-url').value.trim() } });
      $('ex-json').value = JSON.stringify(r.config, null, 2);
      $('ex-notes').innerHTML = r.notes.map(n => `<div class="small warn-text">${icon('warning')} ${esc(n)}</div>`).join('');
      $('ex-out').hidden = false;
    } catch (e) { msg.className = 'small error'; msg.textContent = e.message; }
  });

  $('ex-copy').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText($('ex-json').value); toast('Copied'); }
    catch (e) { $('ex-json').select(); toast('Select all and copy: the browser blocked the clipboard here', true); }
  });
  $('ex-download').addEventListener('click', () => {
    const name = ($('ex-name').value.trim() || 'config').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'config';
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([$('ex-json').value + '\n'], { type: 'application/json' }));
    a.download = `${name}.json`; a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  });
  $('ex-try').addEventListener('click', () => {
    $('cc-text').value = $('ex-json').value;
    tab('import');
    $('import').scrollIntoView({ behavior: 'smooth' });
    check({});
  });

  // Import and Export are tabs; the address keeps the one open (#import, #export)
  const tabs = [...document.querySelectorAll('[data-cc-tab]')];
  function tab(name, focus) {
    for (const t of tabs) {
      const on = t.dataset.ccTab === name;
      t.setAttribute('aria-selected', String(on)); t.tabIndex = on ? 0 : -1;
      $(t.getAttribute('aria-controls')).hidden = !on;
      if (on && focus) t.focus();
    }
    if (location.hash !== '#' + name) history.replaceState(null, '', '#' + name);
  }
  tabs.forEach((t, i) => {
    t.addEventListener('click', () => tab(t.dataset.ccTab));
    t.addEventListener('keydown', e => {
      const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key]; if (!step) return;
      e.preventDefault(); tab(tabs[(i + step + tabs.length) % tabs.length].dataset.ccTab, true);
    });
  });
  addEventListener('hashchange', () => { const h = location.hash.slice(1); if (tabs.some(t => t.dataset.ccTab === h)) tab(h); });
  tab(location.hash === '#export' ? 'export' : 'import');

  load();
})();
