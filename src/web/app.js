'use strict';
(() => {
  // ------------------------------------------------------------------ helpers
  const $ = (sel) => document.querySelector(sel);

  /** Build DOM without innerHTML, so log lines and paths can never inject markup. */
  function h(tag, attrs, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === false || v == null) continue;
      if (k === 'class') el.className = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
      else if (['checked', 'disabled', 'value', 'open', 'selected'].includes(k)) el[k] = v;
      else el.setAttribute(k, v === true ? '' : v);
    }
    for (const kid of kids.flat(Infinity)) {
      if (kid == null || kid === false) continue;
      el.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
    }
    return el;
  }

  async function api(path, body) {
    const opts = body === undefined
      ? {}
      : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) };
    const res = await fetch('/api/' + path, opts);
    let data = null;
    try { data = await res.json(); } catch (_) { /* non-JSON error body */ }
    if (!res.ok) throw new Error((data && data.error) || `Request failed (${res.status})`);
    return data;
  }

  let toastTimer = null;
  function toast(message) {
    const el = $('#toast');
    el.textContent = message;
    el.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => el.classList.remove('show'), 2600);
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
      toast('Copied');
    } catch (_) {
      toast('Could not copy - select the text instead');
    }
  }

  function fmtDuration(sec) {
    sec = Math.max(0, Math.round(sec));
    const hr = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
    const pad = (n) => String(n).padStart(2, '0');
    if (hr) return `${hr}h ${pad(m)}m ${pad(s)}s`;
    if (m) return `${m}m ${pad(s)}s`;
    return `${s}s`;
  }

  function fmtBytes(n) {
    if (n >= 1 << 30) return (n / (1 << 30)).toFixed(1) + ' GiB';
    if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + ' MiB';
    if (n >= 1 << 10) return (n / (1 << 10)).toFixed(1) + ' KiB';
    return n + ' B';
  }

  const riskLabel = { caution: 'Caution', risky: 'Risky' };
  const riskPill = (risk) => (riskLabel[risk] ? h('span', { class: 'pill ' + risk }, riskLabel[risk]) : null);

  // -------------------------------------------------------------------- state
  const STORE = 'kertweaker.v1';
  const S = {
    system: null,
    catalog: null,
    releases: null,
    trees: [],
    saved: [],
    view: 'start',
    search: '',
    selections: {},
    extra: '',
    source: { mode: 'download', version: '', path: '' },
    base: { mode: '', path: '', text: '' },
    build: { jobs: 0, output: '', llvm: false, clean: false },
    acceptRisk: false,
    preview: null,
    previewError: null,
    job: null,
    jobId: null,
  };

  function persist() {
    try {
      localStorage.setItem(STORE, JSON.stringify({
        selections: S.selections, extra: S.extra, source: S.source, base: S.base, build: S.build,
      }));
    } catch (_) { /* private mode: state just lasts for this tab */ }
  }

  function restore() {
    try {
      const saved = JSON.parse(localStorage.getItem(STORE) || 'null');
      if (!saved) return;
      for (const key of ['selections', 'source', 'base', 'build']) {
        if (saved[key] && typeof saved[key] === 'object') Object.assign(S[key], saved[key]);
      }
      if (typeof saved.extra === 'string') S.extra = saved.extra;
    } catch (_) { /* corrupt entry: start clean */ }
  }

  const tweakById = (id) => S.catalog.tweaks.find((t) => t.id === id);
  const isFree = (t) => t.kind === 'text' || t.kind === 'number';

  function isChanged(t) {
    const v = S.selections[t.id];
    return v != null && v !== '' && v !== 'keep';
  }
  const changedTweaks = () => S.catalog.tweaks.filter(isChanged);

  /** Drop anything the catalog no longer knows, e.g. after an upgrade. */
  function sanitizeSelections(sel) {
    const out = {};
    for (const [id, value] of Object.entries(sel || {})) {
      const t = tweakById(id);
      if (!t || typeof value !== 'string') continue;
      if (isFree(t) || t.options.some((o) => o.id === value)) out[id] = value;
    }
    return out;
  }

  function overallRisk() {
    let worst = 'safe';
    const rank = { safe: 0, caution: 1, risky: 2 };
    for (const t of changedTweaks()) {
      if (isFree(t)) continue;
      const o = t.options.find((x) => x.id === S.selections[t.id]);
      if (o && rank[o.risk] > rank[worst]) worst = o.risk;
    }
    return worst;
  }

  function setSelection(id, value) {
    if (value == null || value === '' || value === 'keep') delete S.selections[id];
    else S.selections[id] = value;
    S.acceptRisk = false;
    persist();
    schedulePreview();
  }

  function go(view) {
    S.view = view;
    S.search = '';
    $('#search').value = '';
    render();
    window.scrollTo({ top: 0 });
    $('#main').focus({ preventScroll: true });
    if (view === 'review') schedulePreview(0);
  }

  // ------------------------------------------------------------------ preview
  let previewTimer = null;
  let previewSeq = 0;
  function schedulePreview(delay = 300) {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(async () => {
      const seq = ++previewSeq;
      try {
        const p = await api('preview', {
          selections: S.selections, extra: S.extra, base: S.base, source: S.source,
        });
        if (seq !== previewSeq) return; // a newer request superseded this one
        S.preview = p;
        S.previewError = null;
      } catch (e) {
        if (seq !== previewSeq) return;
        S.previewError = e.message;
      }
      if (S.view === 'review') renderMain();
    }, delay);
  }

  // ------------------------------------------------------------------- chrome
  function renderChip() {
    const s = S.system;
    $('#machine-chip').textContent =
      `${s.distro} · Linux ${s.running_kernel} · ${s.cpu.model} · ${s.cpu.threads} threads · ${s.memory_gib} GiB RAM`;
  }

  function renderNav() {
    const item = (view, label, extra) =>
      h('button', {
        class: 'nav-item' + (S.view === view && !S.search ? ' active' : ''),
        'aria-current': S.view === view && !S.search ? 'page' : null,
        onclick: () => go(view),
      }, h('span', {}, label), extra);

    const kids = [h('div', { class: 'nav-label' }, 'Setup'), item('start', 'Start')];
    kids.push(h('div', { class: 'nav-label' }, 'Tweaks'));
    for (const g of S.catalog.groups) {
      const n = S.catalog.tweaks.filter((t) => t.group === g.id && isChanged(t)).length;
      kids.push(item(g.id, g.title, n ? h('span', { class: 'badge' }, n) : null));
    }
    kids.push(h('div', { class: 'nav-label' }, 'Finish'));
    kids.push(item('review', 'Review & build'));
    if (S.job) {
      const running = S.job.status === 'running';
      kids.push(item('build', running ? 'Build in progress' : 'Last build',
        h('span', { class: 'dot' + (running ? ' pulse' : '') })));
    }
    $('#nav').replaceChildren(...kids);
  }

  function renderSummary() {
    const changed = changedTweaks();
    const risk = overallRisk();
    const list = changed.map((t) => {
      const value = S.selections[t.id];
      const label = isFree(t) ? value : (t.options.find((o) => o.id === value) || {}).label;
      return h('li', {},
        h('span', { class: 't' }, t.title),
        h('button', {
          class: 'x', title: 'Reset ' + t.title, 'aria-label': 'Reset ' + t.title,
          onclick: () => { setSelection(t.id, null); render(); },
        }, '×'),
        h('span', { class: 'o' }, label));
    });

    $('#summary').replaceChildren(h('div', { class: 'card' },
      h('h2', {}, 'Your changes'),
      changed.length
        ? [
            h('div', { class: 'totals' },
              h('span', { class: 'pill accent' }, `${changed.length} tweak${changed.length === 1 ? '' : 's'}`),
              riskPill(risk)),
            h('ul', { class: 'change-list' }, list),
          ]
        : h('p', { class: 'empty' }, 'Nothing changed yet. Pick a profile on the Start page or flip a tweak.'),
      h('div', { class: 'actions' },
        h('button', { class: 'btn primary', onclick: () => go('review') }, 'Review & build'),
        changed.length ? h('button', {
          class: 'btn ghost',
          onclick: () => { S.selections = {}; S.acceptRisk = false; persist(); schedulePreview(); render(); },
        }, 'Clear all') : null)));
    // Spacing for the action row lives in CSS; mark it for the stylesheet.
    $('#summary').querySelector('.actions').classList.add('summary-actions');
  }

  // --------------------------------------------------------------- tweak card
  function optionRow(t, o, current) {
    const selected = current === o.id;
    const sets = Object.entries(o.sets);
    return h('label', { class: 'option' + (selected ? ' selected' : '') },
      h('input', {
        type: 'radio', name: 'tweak-' + t.id, checked: selected,
        onchange: () => { setSelection(t.id, o.id); render(); },
      }),
      h('span', { class: 'label' }, o.label, ' ', riskPill(o.risk)),
      o.description ? h('span', { class: 'sub' }, o.description) : null,
      selected && o.note ? h('span', { class: 'note ' + o.risk }, o.note) : null,
      selected && sets.length
        ? h('span', { class: 'symbols' }, sets.map(([sym, val]) =>
            h('span', { class: 'sym' + (val == null ? ' off' : '') },
              val == null ? sym : `${sym}=${val}`)))
        : null);
  }

  function tweakCard(t) {
    const current = S.selections[t.id] || 'keep';
    const body = [];
    if (isFree(t)) {
      body.push(h('div', { class: 'options' },
        h('label', { class: 'field' },
          h('span', {}, 'Writes ', h('code', {}, t.symbol), '. Leave empty to keep the base value.'),
          h('input', {
            type: t.kind === 'number' ? 'number' : 'text',
            min: t.kind === 'number' ? 1 : null,
            placeholder: t.placeholder,
            value: S.selections[t.id] || '',
            // Update state on every keystroke, but only redraw the chrome so
            // the field keeps focus while typing.
            oninput: (e) => { setSelection(t.id, e.target.value.trim()); renderNav(); renderSummary(); },
            onchange: () => render(),
          }))));
    } else {
      body.push(h('div', { class: 'options', role: 'radiogroup', 'aria-label': t.title },
        t.options.map((o) => optionRow(t, o, current))));
    }
    return h('section', { class: 'card' + (isChanged(t) ? ' changed' : ''), id: 'tweak-' + t.id },
      h('div', { class: 'card-head' }, h('h3', {}, t.title), riskPill(t.risk)),
      h('p', { class: 'desc' }, t.description,
        t.docs ? [' ', h('a', { href: t.docs, target: '_blank', rel: 'noopener noreferrer' }, 'Kernel docs')] : null),
      body);
  }

  // -------------------------------------------------------------------- views
  function viewStart() {
    const s = S.system;
    const fact = (k, v) => h('div', { class: 'fact' }, h('div', { class: 'k' }, k), h('div', { class: 'v' }, v));
    const pk = s.packages;

    const machine = h('section', { class: 'card stack' },
      h('h2', {}, 'Your machine'),
      h('div', { class: 'facts' },
        fact('Distribution', s.distro),
        fact('Running kernel', s.running_kernel),
        fact('Processor', `${s.cpu.model} (${s.cpu.threads} threads)`),
        fact('Memory', `${s.memory_gib} GiB`),
        fact('Free disk for builds', `${s.free_disk_gib} GiB`),
        fact('Compiler', s.gcc_version ? `gcc ${s.gcc_version}` : 'gcc not found'),
        fact('Architecture', s.arch),
        fact('Secure Boot', s.secure_boot == null ? 'unknown' : s.secure_boot ? 'on' : 'off')),
      s.warnings.map((w) => h('div', { class: 'notice ' + w.level },
        h('span', { class: 'tag' }, w.level === 'error' ? 'Blocked' : 'Heads up'), h('span', {}, w.text))),
      pk.missing_required.length && pk.install_command
        ? h('div', { class: 'stack' },
            h('span', {}, 'Install the missing build tools with:'),
            h('div', { class: 'cmd-row' },
              h('code', {}, pk.install_command),
              h('button', { class: 'btn small', onclick: () => copy(pk.install_command) }, 'Copy')))
        : null,
      !pk.missing_required.length
        ? h('div', { class: 'notice good' }, h('span', { class: 'tag' }, 'Ready'),
            h('span', {}, 'Every required build tool is installed.',
              pk.missing_optional.length ? ` Optional and not installed: ${pk.missing_optional.join(', ')}.` : ''))
        : null);

    // ------------------------------------------------------------- profiles
    const sameAs = (sel) => {
      const a = Object.entries(S.selections), b = Object.entries(sel);
      return a.length === b.length && b.every(([k, v]) => S.selections[k] === v);
    };
    const applyProfile = (title, sel, extra) => {
      S.selections = sanitizeSelections(sel);
      if (extra !== undefined) S.extra = extra;
      S.acceptRisk = false;
      persist();
      schedulePreview();
      render();
      toast(`Applied "${title}" - ${Object.keys(S.selections).length} tweaks set`);
    };
    const profiles = h('section', { class: 'stack' },
      h('div', { class: 'section-head' },
        h('span', { class: 'step' }, 'Step 1'),
        h('h2', {}, 'Pick a starting profile'),
        h('p', { class: 'lede' }, 'A profile is just a set of tweaks. Apply one, then adjust anything you like - or skip this and start from nothing.')),
      h('div', { class: 'grid' }, S.catalog.profiles.map((p) =>
        h('button', {
          class: 'profile' + (sameAs(p.selections) ? ' active' : ''),
          onclick: () => applyProfile(p.title, p.selections),
        },
          h('span', { class: 'row' },
            h('h3', {}, p.title),
            s.suggested_profile === p.id ? h('span', { class: 'pill accent' }, 'Suggested') : null),
          h('span', { class: 'what' }, p.description),
          h('span', { class: 'who' }, p.recommended_for)))),
      S.saved.length
        ? h('div', { class: 'stack' },
            h('h3', {}, 'Your saved tweak sets'),
            h('div', { class: 'chips' }, S.saved.map((p) =>
              h('span', { class: 'chip' },
                h('span', { onclick: () => applyProfile(p.name, p.selections, p.extra || '') }, p.name),
                ' ',
                h('button', {
                  class: 'x', title: 'Delete ' + p.name, 'aria-label': 'Delete ' + p.name,
                  onclick: async () => {
                    await api('saved/delete', { name: p.name });
                    S.saved = await api('saved');
                    render();
                  },
                }, '×')))))
        : null);

    // --------------------------------------------------------------- source
    const rel = S.releases;
    const radio = (group, value, current, onpick, label, sub, extra, disabled) =>
      h('label', { class: 'option' + (current === value ? ' selected' : '') },
        h('input', { type: 'radio', name: group, checked: current === value, disabled, onchange: onpick }),
        h('span', { class: 'label' }, label),
        sub ? h('span', { class: 'sub' }, sub) : null,
        current === value && extra ? h('span', { class: 'sub' }, extra) : null);

    const setSource = (patch) => { Object.assign(S.source, patch); persist(); schedulePreview(); };
    const source = h('section', { class: 'card' },
      h('span', { class: 'step' }, 'Step 2'),
      h('h2', {}, 'Kernel source'),
      h('div', { class: 'options' },
        radio('source', 'download', S.source.mode, () => { setSource({ mode: 'download' }); render(); },
          'Download from kernel.org',
          'Fetched once and cached. About 150 MB to download, 1.5 GB unpacked.',
          rel
            ? h('span', { class: 'stack' },
                h('select', {
                  'aria-label': 'Kernel version',
                  onchange: (e) => setSource({ version: e.target.value }),
                }, rel.releases.map((r) =>
                  h('option', { value: r.version, selected: r.version === S.source.version },
                    `${r.version} - ${r.moniker}${r.version === rel.latest_stable ? ' (latest stable)' : ''}${r.eol ? ' (end of life)' : ''}`))),
                rel.online === false
                  ? h('span', { class: 'note' }, 'Could not reach kernel.org, so this is a built-in list. Check your connection before building.')
                  : null)
            : 'Loading versions...'),
        radio('source', 'local', S.source.mode, () => { setSource({ mode: 'local' }); render(); },
          'Use a source tree already on this machine',
          'A git checkout or an unpacked tarball.',
          h('span', { class: 'stack' },
            h('input', {
              type: 'text', placeholder: '~/src/linux', value: S.source.path, 'aria-label': 'Path to kernel tree',
              oninput: (e) => setSource({ path: e.target.value }),
            }),
            S.trees.length
              ? h('span', { class: 'chips' }, S.trees.map((t) =>
                  h('button', { class: 'chip', onclick: () => { setSource({ path: t.path }); render(); } },
                    `Linux ${t.version} - ${t.path}`)))
              : null))));

    // ----------------------------------------------------------------- base
    const setBase = (patch) => { Object.assign(S.base, patch); persist(); schedulePreview(); };
    const pickBase = (mode) => () => { setBase({ mode }); render(); };
    const base = h('section', { class: 'card' },
      h('span', { class: 'step' }, 'Step 3'),
      h('h2', {}, 'Starting config'),
      h('p', { class: 'desc' }, 'Your tweaks are applied on top of this. Starting from the running kernel keeps every driver this machine already uses.'),
      h('div', { class: 'options' },
        radio('base', 'running', S.base.mode, pickBase('running'),
          "This machine's running kernel",
          s.running_config
            ? `Read from ${s.running_config}. The safest starting point.`
            : 'Not available: this system does not expose its kernel config.',
          null, !s.running_config),
        radio('base', 'defconfig', S.base.mode, pickBase('defconfig'),
          'Kernel defaults (defconfig)',
          'Small and generic. Quick to build, but may lack drivers for your hardware.'),
        S.source.mode === 'local'
          ? radio('base', 'tree', S.base.mode, pickBase('tree'),
              "The tree's existing .config", 'Continue from whatever is already configured there.')
          : null,
        radio('base', 'file', S.base.mode, pickBase('file'),
          'A config file', 'For example a distro config from /boot.',
          h('input', {
            type: 'text', placeholder: '/boot/config-6.8.0-45-generic', value: S.base.path,
            'aria-label': 'Path to config file', oninput: (e) => setBase({ path: e.target.value }),
          })),
        radio('base', 'text', S.base.mode, pickBase('text'),
          'Paste a config', null,
          h('textarea', {
            placeholder: 'CONFIG_...', 'aria-label': 'Pasted config',
            oninput: (e) => setBase({ text: e.target.value }),
          }, S.base.text))));

    return [
      h('div', { class: 'section-head' },
        h('h1', {}, 'Build a kernel for this machine'),
        h('p', { class: 'lede' }, 'Choose what you want in plain terms, see exactly which CONFIG_ options that changes, then build it. Nothing is installed unless you ask.')),
      machine, profiles, source, base,
      h('div', { class: 'actions' },
        h('button', { class: 'btn primary', onclick: () => go(S.catalog.groups[0].id) }, 'Start tweaking'),
        h('button', { class: 'btn', onclick: () => go('review') }, 'Skip to review')),
    ];
  }

  function viewGroup(group) {
    const idx = S.catalog.groups.indexOf(group);
    const next = S.catalog.groups[idx + 1];
    return [
      h('div', { class: 'section-head' }, h('h1', {}, group.title), h('p', { class: 'lede' }, group.blurb)),
      S.catalog.tweaks.filter((t) => t.group === group.id).map(tweakCard),
      h('div', { class: 'actions' },
        next
          ? h('button', { class: 'btn primary', onclick: () => go(next.id) }, `Next: ${next.title}`)
          : h('button', { class: 'btn primary', onclick: () => go('review') }, 'Review & build')),
    ];
  }

  function viewSearch() {
    const q = S.search.toLowerCase();
    const hits = S.catalog.tweaks.filter((t) =>
      [t.title, t.description, t.symbol, ...t.tags,
        ...t.options.flatMap((o) => [o.label, o.description, ...Object.keys(o.sets)])]
        .join(' ').toLowerCase().includes(q));
    return [
      h('div', { class: 'section-head' },
        h('h1', {}, `Search: ${S.search}`),
        h('p', { class: 'lede' }, hits.length
          ? `${hits.length} tweak${hits.length === 1 ? '' : 's'} match.`
          : 'No tweak matches. For a symbol that is not in the catalog, add it under "Extra options" on the Review page.')),
      hits.map(tweakCard),
    ];
  }

  function viewReview() {
    const s = S.system;
    const p = S.preview;
    const risk = overallRisk();
    const out = [
      h('div', { class: 'section-head' },
        h('h1', {}, 'Review & build'),
        h('p', { class: 'lede' }, 'Check what changes, choose how to build, then start. kbuild resolves dependencies first and you are told about anything that did not stick.')),
    ];

    if (S.previewError) {
      out.push(h('div', { class: 'notice error' }, h('span', { class: 'tag' }, 'Error'), S.previewError));
    }

    if (p) {
      const r = p.resolved;
      const notes = r.notes.map((n) =>
        h('div', { class: 'notice ' + (n.level === 'safe' ? '' : n.level) },
          h('span', { class: 'tag' }, n.level === 'risky' ? 'Risky' : n.level === 'error' ? 'Invalid' : 'Note'),
          h('span', {}, n.text)));
      const conflicts = r.conflicts.map((c) =>
        h('div', { class: 'notice caution' }, h('span', { class: 'tag' }, 'Override'),
          h('span', {}, 'Your extra options set ', h('code', {}, `${c.symbol}=${c.winning_value}`),
            `, replacing ${c.previous_value} from "${(tweakById(c.previous_tweak) || {}).title || c.previous_tweak}".`)));
      if (notes.length || conflicts.length) out.push(h('div', { class: 'stack' }, notes, conflicts));

      // ------------------------------------------------------------- diff
      const diffCard = h('section', { class: 'card stack' }, h('h2', {}, 'What will change'));
      if (p.base.available) {
        diffCard.append(h('p', { class: 'desc' },
          `${p.change_count} of the ${r.symbol_count} requested options differ from ${p.base.description} `,
          `(${p.base.enabled} options on, ${p.base.modules} as modules).`,
          p.already_set ? ` ${p.already_set} are already set the way you want.` : ''));
        if (p.changes.length) {
          diffCard.append(h('div', { class: 'table-wrap' }, h('table', {},
            h('thead', {}, h('tr', {}, h('th', {}, 'Before'), h('th', {}, 'After'), h('th', {}, 'Because of'))),
            h('tbody', {}, p.changes.map((c) =>
              h('tr', { class: c.kind },
                h('td', { class: 'mono before' }, c.before || '(absent)'),
                h('td', { class: 'mono after' }, c.after),
                h('td', {}, c.tweak === 'extra_options' ? 'Extra options'
                  : (tweakById(c.tweak) || {}).title || c.tweak)))))));
          if (p.change_count > p.changes.length) {
            diffCard.append(h('p', { class: 'desc' }, `Showing the first ${p.changes.length} of ${p.change_count}.`));
          }
        } else if (r.symbol_count) {
          diffCard.append(h('div', { class: 'notice good' }, h('span', { class: 'tag' }, 'No change'),
            'The starting config already has every option you asked for.'));
        } else {
          diffCard.append(h('p', { class: 'empty' }, 'No tweaks selected, so the build will use the starting config as it is.'));
        }
      } else {
        diffCard.append(h('div', { class: 'notice' }, h('span', { class: 'tag' }, 'No comparison'), h('span', {}, p.base.error)));
      }
      if (r.localmodconfig) {
        diffCard.append(h('div', { class: 'notice caution' }, h('span', { class: 'tag' }, 'Trim'),
          `Before your tweaks are applied, the config is trimmed to the ${s.loaded_module_count} modules loaded right now. Plug in anything you want supported first.`));
      }
      if (r.symbol_count) {
        diffCard.append(h('details', {},
          h('summary', {}, `Config fragment (${r.symbol_count} options)`),
          h('div', { class: 'stack' },
            h('pre', { class: 'fragment' }, r.fragment_text),
            h('div', { class: 'actions' },
              h('button', { class: 'btn small', onclick: () => copy(r.fragment_text) }, 'Copy fragment')))));
      }
      out.push(diffCard);
    } else if (!S.previewError) {
      out.push(h('p', { class: 'loading' }, 'Working out what changes...'));
    }

    // ------------------------------------------------------------- extras
    out.push(h('section', { class: 'card stack' },
      h('h2', {}, 'Extra options'),
      h('p', { class: 'desc' }, 'For anything the catalog does not cover: raw config lines, one per line. They win over the tweaks above, and each override is listed.'),
      h('textarea', {
        placeholder: 'CONFIG_USB_SERIAL=m\n# CONFIG_BT is not set', 'aria-label': 'Extra config options',
        oninput: (e) => { S.extra = e.target.value; persist(); schedulePreview(600); },
      }, S.extra)));

    // --------------------------------------------------------------- save
    const nameInput = h('input', { type: 'text', placeholder: 'e.g. my-desktop', 'aria-label': 'Name for this tweak set' });
    out.push(h('section', { class: 'card stack' },
      h('h2', {}, 'Save this tweak set'),
      h('p', { class: 'desc' }, 'Keep it for the next kernel release. Saved sets show up on the Start page.'),
      h('div', { class: 'inline' }, nameInput,
        h('button', {
          class: 'btn',
          onclick: async () => {
            try {
              await api('saved', { name: nameInput.value, selections: S.selections, extra: S.extra });
              S.saved = await api('saved');
              toast('Saved');
            } catch (e) { toast(e.message); }
          },
        }, 'Save'))));

    // -------------------------------------------------------- build options
    const b = S.build;
    const forcedClang = !!(p && p.resolved.needs_clang);
    const setBuild = (patch) => { Object.assign(S.build, patch); persist(); };
    const output = (value, label, sub) =>
      h('label', { class: 'option' + (b.output === value ? ' selected' : '') },
        h('input', { type: 'radio', name: 'output', checked: b.output === value, onchange: () => { setBuild({ output: value }); render(); } }),
        h('span', { class: 'label' }, label), h('span', { class: 'sub' }, sub));
    out.push(h('section', { class: 'card stack' },
      h('h2', {}, 'Build options'),
      h('div', { class: 'row-2' },
        h('label', { class: 'field' },
          h('span', {}, `Parallel jobs (this machine has ${s.cpu.threads} threads; ${s.suggested_jobs} suggested for its RAM)`),
          h('input', {
            type: 'number', min: 1, max: 256, value: b.jobs,
            oninput: (e) => setBuild({ jobs: Math.max(1, Math.min(256, parseInt(e.target.value, 10) || 1)) }),
          }))),
      h('div', { class: 'options', role: 'radiogroup', 'aria-label': 'Output format' },
        output('image', 'Kernel image + modules', 'Built in the tree. You install it with make modules_install install.'),
        output('deb', 'Debian packages (.deb)' + (s.package_family === 'deb' ? ' - recommended here' : ''),
          'Installs and uninstalls cleanly with dpkg, and your bootloader updates itself.'),
        output('rpm', 'RPM packages' + (s.package_family === 'rpm' ? ' - recommended here' : ''),
          'For Fedora, RHEL and openSUSE.')),
      h('label', { class: 'check' },
        h('input', {
          type: 'checkbox', checked: b.llvm || forcedClang, disabled: forcedClang,
          onchange: (e) => setBuild({ llvm: e.target.checked }),
        }),
        h('span', {}, 'Build with clang (LLVM=1)', h('span', { class: 'sub' },
          forcedClang
            ? 'Switched on by your link-time optimisation tweak, which only clang supports.'
            : 'Use clang and lld instead of gcc. Both need to be installed.'))),
      h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: b.clean, onchange: (e) => setBuild({ clean: e.target.checked }) }),
        h('span', {}, 'Clean the tree first', h('span', { class: 'sub' }, 'Slower, but rules out stale objects after big config changes.')))));

    if (risk === 'risky') {
      out.push(h('label', { class: 'notice risky check' },
        h('input', { type: 'checkbox', checked: S.acceptRisk, onchange: (e) => { S.acceptRisk = e.target.checked; renderMain(); } }),
        h('span', {}, h('strong', {}, 'This selection includes risky options. '),
          'They can leave this machine unbootable or less secure. I have read the notes above and want to build anyway.')));
    }

    const blocked = s.packages.missing_required.length > 0;
    const busy = S.job && S.job.status === 'running';
    const cannot = (risk === 'risky' && !S.acceptRisk) || busy;
    if (blocked) {
      out.push(h('div', { class: 'notice error' }, h('span', { class: 'tag' }, 'Blocked'),
        h('span', {}, `Building needs: ${s.packages.missing_required.join(', ')}. You can still write the config. Install command: `,
          h('code', {}, s.packages.install_command || 'see your distribution'))));
    }
    out.push(h('div', { class: 'actions' },
      h('button', { class: 'btn primary', disabled: cannot || blocked, onclick: () => startBuild(false) }, 'Build kernel'),
      h('button', { class: 'btn', disabled: cannot, onclick: () => startBuild(true) }, 'Write config only'),
      busy ? h('span', { class: 'empty' }, 'A build is already running.') : null));
    return out;
  }

  // -------------------------------------------------------------------- build
  async function startBuild(configOnly) {
    try {
      const res = await api('build', {
        source: S.source, base: S.base, selections: S.selections, extra: S.extra,
        jobs: S.build.jobs, output: S.build.output, llvm: S.build.llvm, clean: S.build.clean,
        config_only: configOnly, accept_risk: S.acceptRisk,
      });
      attach(res.id);
      go('build');
    } catch (e) {
      toast(e.message);
    }
  }

  let stream = null;
  let logLines = 0;
  let lastUpdate = 0;
  const B = {}; // persistent build-view nodes, so updates never reset the log scroll

  function attach(id) {
    if (stream) stream.close();
    S.jobId = id;
    S.job = null;
    logLines = 0;
    B.root = null;
    open(0);
  }

  function open(from) {
    stream = new EventSource(`/api/jobs/${encodeURIComponent(S.jobId)}/events?from=${from}`);
    stream.addEventListener('update', (e) => onUpdate(JSON.parse(e.data)));
    stream.addEventListener('end', () => stream.close());
    stream.onerror = () => {
      // EventSource would reconnect with the original cursor and replay the
      // log, so reconnect by hand from where we actually are.
      stream.close();
      if (S.job && S.job.status === 'running') setTimeout(() => open(S.job.log_total || 0), 1000);
    };
  }

  function onUpdate(job) {
    const wasRunning = !S.job || S.job.status === 'running';
    S.job = job;
    lastUpdate = Date.now();
    if (S.view === 'build' && !S.search) {
      if (!B.root) renderMain(); else updateBuild(job.log);
    } else {
      // Lines are only kept in the DOM, so remember to refetch on next visit.
      B.root = null;
    }
    if (wasRunning && job.status !== 'running') {
      renderNav();
      toast(job.status === 'done' ? 'Build finished' : job.status === 'failed' ? 'Build failed' : 'Build cancelled');
    } else if (!$('#nav .dot')) {
      renderNav();
    }
  }

  const stageIcon = { pending: '·', running: '›', done: '✓', failed: '✕', skipped: '–' };

  function viewBuild() {
    if (!S.job) return [h('p', { class: 'loading' }, 'Connecting to the build...')];
    B.title = h('h1', {});
    B.pill = h('span', { class: 'pill' });
    B.cancel = h('button', {
      class: 'btn danger',
      onclick: async () => {
        B.cancel.disabled = true;
        try { await api(`jobs/${S.jobId}/cancel`, {}); } catch (e) { toast(e.message); }
      },
    }, 'Cancel build');
    B.meter = h('div', { class: 'meter' }, h('i', {}));
    B.meterLeft = h('span', {});
    B.meterRight = h('span', {});
    B.stages = h('ul', { class: 'stages' });
    B.log = h('pre', { class: 'log', tabindex: 0, 'aria-label': 'Build log' });
    B.result = h('div', { class: 'stack' });
    B.root = true;
    logLines = 0;

    // The page may have been reloaded mid-build: fetch the log so far.
    api(`jobs/${S.jobId}?from=0`).then((full) => { if (B.root) { B.log.replaceChildren(); logLines = 0; updateBuild(full.log, full); } });

    const nodes = [
      h('div', { class: 'build-head' }, B.title, B.pill, B.cancel),
      h('section', { class: 'card' }, B.meter, h('div', { class: 'meter-row' }, B.meterLeft, B.meterRight)),
      B.result,
      h('section', { class: 'card' }, h('h2', {}, 'Stages'), B.stages),
      h('section', { class: 'card stack' }, h('h2', {}, 'Log'), B.log),
    ];
    updateBuild([]);
    return nodes;
  }

  function appendLog(lines) {
    if (!lines || !lines.length) return;
    const follow = B.log.scrollHeight - B.log.scrollTop - B.log.clientHeight < 40;
    const frag = document.createDocumentFragment();
    for (const line of lines) {
      const cls = /\berror\b|\*\*\*|^make.*Error /i.test(line) ? 'err'
        : /\bwarning\b/i.test(line) ? 'warn'
        : line.startsWith('$ ') ? 'cmd' : '';
      frag.append(cls ? h('span', { class: cls }, line + '\n') : document.createTextNode(line + '\n'));
    }
    B.log.append(frag);
    logLines += lines.length;
    while (B.log.childNodes.length > 4000) B.log.firstChild.remove();
    if (follow) B.log.scrollTop = B.log.scrollHeight;
  }

  function updateBuild(newLines, snapshot) {
    const job = snapshot || S.job;
    if (!job || !B.root) return;
    appendLog(newLines);

    const running = job.status === 'running';
    const tree = job.tree ? job.tree.split('/').pop() : 'kernel';
    B.title.textContent = running ? `Building ${tree}` : job.status === 'done' ? `Built ${tree}`
      : job.status === 'failed' ? 'Build failed' : 'Build cancelled';
    B.pill.className = 'pill ' + job.status;
    B.pill.textContent = job.status;
    B.cancel.hidden = !running;

    // Progress: a real percentage only when a previous build tells us how
    // many objects to expect. Otherwise say what we know and no more.
    const bar = B.meter.firstChild;
    const building = job.stages.some((st) => st.id === 'build' && st.state === 'running');
    if (!running) {
      B.meter.classList.remove('indeterminate');
      bar.style.width = job.status === 'done' ? '100%' : bar.style.width || '0%';
    } else if (building && job.objects_expected) {
      B.meter.classList.remove('indeterminate');
      bar.style.width = Math.min(99, (job.objects / job.objects_expected) * 100).toFixed(1) + '%';
    } else {
      B.meter.classList.add('indeterminate');
      bar.style.width = '';
    }
    const elapsed = job.elapsed + (running ? (Date.now() - lastUpdate) / 1000 : 0);
    B.meterLeft.textContent = building
      ? `${job.phase} - ${job.objects} objects` +
        (job.objects_expected ? ` of about ${job.objects_expected} (from your last build)` : '')
      : job.phase;
    B.meterRight.textContent = fmtDuration(elapsed);

    B.stages.replaceChildren(...job.stages.map((st) =>
      h('li', { class: st.state },
        h('span', { class: 'icon' }, stageIcon[st.state]),
        h('span', { class: 'title' }, st.title),
        h('span', { class: 'time' }, st.seconds != null && st.state !== 'pending' && st.state !== 'skipped' ? fmtDuration(st.seconds) : ''),
        st.detail ? h('span', { class: 'detail' }, st.detail) : null)));

    // ---------------------------------------------------------- results
    const res = [];
    if (job.status === 'failed') {
      res.push(h('div', { class: 'notice error' }, h('span', { class: 'tag' }, 'Failed'),
        h('span', {}, job.error, ' The first line in the log containing "error" is usually the real cause. Full log: ',
          h('code', {}, job.log_path))));
    }
    for (const line of job.advice || []) {
      res.push(h('div', { class: 'notice' }, h('span', { class: 'tag' }, 'Note'), h('span', {}, line)));
    }
    if (job.unapplied.length) {
      res.push(h('section', { class: 'card stack' },
        h('h2', {}, `${job.unapplied.length} option${job.unapplied.length === 1 ? '' : 's'} did not stick`),
        h('p', { class: 'desc' }, 'kbuild overrode these while resolving dependencies - usually because the option does not exist in this kernel version, or something it depends on is off.'),
        h('div', { class: 'table-wrap' }, h('table', {},
          h('thead', {}, h('tr', {}, h('th', {}, 'You asked for'), h('th', {}, 'You got'))),
          h('tbody', {}, job.unapplied.map((u) =>
            h('tr', {}, h('td', { class: 'mono' }, u.wanted), h('td', { class: 'mono after' }, u.actual))))))));
    }
    if (job.status === 'done') {
      if (job.artifacts.length) {
        res.push(h('section', { class: 'card stack' },
          h('h2', {}, 'What was built'),
          h('ul', { class: 'files' }, job.artifacts.map((a) =>
            h('li', {}, h('span', { class: 'path' }, a.path), h('span', { class: 'size' }, fmtBytes(a.bytes)))))));
      }
      if (job.next_steps.length) {
        res.push(h('section', { class: 'card stack' },
          h('h2', {}, job.artifacts.length ? 'Install it' : 'Done'),
          job.artifacts.length
            ? h('p', { class: 'desc' }, 'Run these yourself, in order. Keep your current kernel installed so you can pick it from the boot menu if the new one misbehaves.')
            : null,
          job.next_steps.map((cmd) => cmd.startsWith('sudo ')
            ? h('div', { class: 'cmd-row' }, h('code', {}, cmd),
                h('button', { class: 'btn small', onclick: () => copy(cmd.split('   #')[0].trim()) }, 'Copy'))
            : h('p', {}, cmd))));
      }
    }
    if (job.config_path) {
      res.push(h('div', { class: 'actions' },
        h('a', { class: 'btn', href: `/api/jobs/${encodeURIComponent(job.id)}/config` }, 'Download final .config'),
        !running ? h('button', { class: 'btn', onclick: () => go('review') }, 'Back to review') : null));
    }
    B.result.replaceChildren(...res);
  }

  // Keep the elapsed clock moving between server updates.
  setInterval(() => {
    if (S.view === 'build' && B.root && S.job && S.job.status === 'running') {
      B.meterRight.textContent = fmtDuration(S.job.elapsed + (Date.now() - lastUpdate) / 1000);
    }
  }, 1000);

  // ------------------------------------------------------------------- render
  function renderMain() {
    let nodes;
    if (S.search) nodes = viewSearch();
    else if (S.view === 'start') nodes = viewStart();
    else if (S.view === 'review') nodes = viewReview();
    else if (S.view === 'build') nodes = viewBuild();
    else nodes = viewGroup(S.catalog.groups.find((g) => g.id === S.view) || S.catalog.groups[0]);
    if (S.view !== 'build' || S.search) B.root = null;
    $('#main').replaceChildren(...nodes.flat(Infinity).filter(Boolean));
  }

  function render() {
    renderNav();
    renderMain();
    renderSummary();
  }

  // --------------------------------------------------------------------- boot
  async function boot() {
    restore();
    try {
      [S.system, S.catalog] = await Promise.all([api('system'), api('catalog')]);
    } catch (e) {
      $('#main').replaceChildren(h('div', { class: 'notice error' },
        h('span', { class: 'tag' }, 'Error'), `Could not load: ${e.message}`));
      return;
    }
    S.selections = sanitizeSelections(S.selections);
    if (!S.build.jobs) S.build.jobs = S.system.suggested_jobs;
    if (!S.build.output) {
      S.build.output = { deb: 'deb', rpm: 'rpm' }[S.system.package_family] || 'image';
    }
    if (!S.base.mode || (S.base.mode === 'running' && !S.system.running_config)) {
      S.base.mode = S.system.running_config ? 'running' : 'defconfig';
    }
    renderChip();
    render();

    $('#search').addEventListener('input', (e) => { S.search = e.target.value.trim(); renderNav(); renderMain(); });
    document.addEventListener('keydown', (e) => {
      if (e.key === '/' && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName)) {
        e.preventDefault();
        $('#search').focus();
      }
    });

    api('releases').then((r) => {
      S.releases = r;
      if (!r.releases.some((x) => x.version === S.source.version)) {
        S.source.version = r.latest_stable || (r.releases[0] || {}).version || '';
      }
      if (S.view === 'start') renderMain();
    }).catch(() => {});
    api('trees').then((t) => { S.trees = t; if (S.view === 'start') renderMain(); }).catch(() => {});
    api('saved').then((p) => { S.saved = p; if (S.view === 'start') renderMain(); }).catch(() => {});
    // Reattach to a build that outlived a page reload.
    api('jobs').then((jobs) => {
      const last = jobs[jobs.length - 1];
      if (!last) return;
      attach(last.id);
      if (last.status === 'running') go('build');
    }).catch(() => {});
  }

  boot();
})();
