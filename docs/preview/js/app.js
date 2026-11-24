(function () {
  'use strict';
  /* ------------------------------------------------------------------ helpers */
  const $ = (s, r) => (r || document).querySelector(s);
  const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const fmt = (n) => Number(n).toLocaleString('en-US', { maximumFractionDigits: 2 });
  const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
  const RMC = window.__RMC = {};
  const reduced = () => !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
  const store = { get(k) { try { return window.localStorage.getItem(k); } catch (e) { return null; } }, set(k, v) { try { window.localStorage.setItem(k, v); } catch (e) { /* storage blocked */ } } };

  /* ------------------------------------------------------------------ tiers, theme, icons */
  const TIER = ['Bronze', 'Silver', 'Gold', 'Platinum', 'Diamond', 'Ruby', 'Emulsion'];
  const KIND = ['bronze', 'silver', 'gold', 'platinum', 'dia', 'ruby'];
  const TVAR = ['var(--m1)', 'var(--m2)', 'var(--m3)', 'var(--m4)', 'var(--m5)', 'var(--m6)', 'var(--m7)'];
  const THEX = ['#c07f45', '#c9cfd8', '#eac35e', '#a9e4e1', '#cbe7ff', '#ff4757', '#b49cff'];
  const XP_TIER = [25, 50, 100, 250, 500, 1000, 2000];
  const LV_TITLES = [[1, 'Rookie'], [3, 'Tinkerer'], [6, 'Builder'], [8, 'Tuner'], [12, 'Overclocker'], [16, 'Engineer'], [20, 'Architect']];
  const RANKS = [[1, 'Bronze', 0], [5, 'Silver', 1], [10, 'Gold', 2], [15, 'Platinum', 3], [20, 'Diamond', 4], [30, 'Ruby', 5], [40, 'Emulsion', 6]];
  const xpNext = (l) => Math.round(60 + 40 * Math.pow(l, 1.25));
  const titleOf = (l) => LV_TITLES.filter((x) => l >= x[0]).pop()[1];
  const rankOf = (l) => RANKS.filter((x) => l >= x[0]).pop();
  const THEME = "THEME = { id: 'uptime-medal', shape: 'round' (uShape 0), metalLabel: 'Uptime', back: 'card' (spec sheet), pocketIcon: false (icon engraved) }";
  const ICONS = {
    cpu: '<rect x="6" y="6" width="12" height="12" rx="2"/><rect x="9.5" y="9.5" width="5" height="5" rx=".6"/><path d="M9 2.5v3M15 2.5v3M9 18.5v3M15 18.5v3M2.5 9h3M2.5 15h3M18.5 9h3M18.5 15h3"/>',
    clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7v5l3.5 2"/>',
    activity: '<path d="M3 12h4l2.5-6 4 12 2.5-6H21"/>',
    flame: '<path d="M12 3c1 3.5 5 5.5 5 10a5 5 0 0 1-10 0c0-2 1-3.2 2-4.2.2 1.4 1 2.2 1.8 2.5C10.5 8.5 11 5.5 12 3z"/>',
    gpu: '<rect x="2.5" y="6.5" width="19" height="10" rx="1.5"/><circle cx="9" cy="11.5" r="2.6"/><circle cx="16" cy="11.5" r="1.4"/><path d="M5 16.5v3M2.5 19.5h6"/>',
    disk: '<ellipse cx="12" cy="6" rx="7.5" ry="3"/><path d="M4.5 6v12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3V6M4.5 12c0 1.7 3.4 3 7.5 3s7.5-1.3 7.5-3"/>',
    globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.6 2.4 4 5.4 4 8.5s-1.4 6.1-4 8.5c-2.6-2.4-4-5.4-4-8.5s1.4-6.1 4-8.5z"/>',
    down: '<path d="M12 4v11M7 10.5l5 5 5-5M5 20h14"/>',
    up: '<path d="M12 20V9M7 13.5l5-5 5 5M5 4h14"/>',
    pad: '<rect x="2.5" y="7.5" width="19" height="10" rx="5"/><path d="M8 10.5v4M6 12.5h4M16 11.5h.01M18 13.5h.01"/>',
    bell: '<path d="M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 2h-15zM10 20.5a2 2 0 0 0 4 0"/>',
    xcircle: '<circle cx="12" cy="12" r="8.5"/><path d="M8.5 8.5l7 7M15.5 8.5l-7 7"/>',
    calendar: '<rect x="3.5" y="5" width="17" height="15" rx="2"/><path d="M3.5 10h17M8 3v4M16 3v4"/>',
    repeat: '<path d="M4 11V9a3 3 0 0 1 3-3h12M16 3l3 3-3 3M20 13v2a3 3 0 0 1-3 3H5M8 21l-3-3 3-3"/>',
    hourglass: '<path d="M7 3h10M7 21h10M8 3c0 5 8 5 8 9s-8 4-8 9M16 3c0 5-8 5-8 9s8 4 8 9"/>',
    power: '<path d="M12 3.5v8M7 6.5a7.5 7.5 0 1 0 10 0"/>',
    screens: '<rect x="2.5" y="5" width="12" height="9" rx="1.5"/><rect x="9.5" y="9" width="12" height="9" rx="1.5"/>',
    refresh: '<path d="M20 11a8 8 0 1 0-1.5 5M20 5v6h-6"/>',
    gauge: '<path d="M4.5 17a8.5 8.5 0 1 1 15 0M12 17l4-5"/>',
    box: '<path d="M3.5 8l8.5-4.5L20.5 8v8L12 20.5 3.5 16zM3.5 8L12 12.5 20.5 8M12 12.5v8"/>',
    flag: '<path d="M5.5 21V4M5.5 5h11l-2.5 3.5 2.5 3.5h-11"/>',
    snow: '<path d="M12 3v18M4.2 7.5l15.6 9M19.8 7.5l-15.6 9"/>',
    stack: '<path d="M12 3.5l8.5 4.5-8.5 4.5L3.5 8zM3.5 12l8.5 4.5 8.5-4.5M3.5 16l8.5 4.5 8.5-4.5"/>',
    star: '<path d="M12 3.5l2.6 5.4 5.9.8-4.3 4.1 1 5.9L12 17l-5.2 2.7 1-5.9L3.5 9.7l5.9-.8z"/>',
    thermo: '<path d="M10 14.5V5a2 2 0 0 1 4 0v9.5a4 4 0 1 1-4 0zM12 9v7"/>',
    ram: '<rect x="3" y="8" width="18" height="8" rx="1"/><path d="M7 11v2M11 11v2M15 11v2M5 16v2M9 16v2M13 16v2M17 16v2"/>',
    nowifi: '<path d="M3.5 9a13 13 0 0 1 17 0M6.5 12.5a8.5 8.5 0 0 1 11 0M9.5 16a4 4 0 0 1 5 0M12 19.5h.01M4 4l16 16"/>',
    shieldx: '<path d="M12 3.5l7 2.5v5.5c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z"/><path d="M9.5 9.5l5 5M14.5 9.5l-5 5"/>',
    battery: '<rect x="2.5" y="8" width="17" height="8" rx="1.5"/><path d="M21.5 11v2M6 11v2"/>',
    moon: '<path d="M19.5 14.5A8 8 0 0 1 9.5 4.5a8 8 0 1 0 10 10z"/>',
    sparkle: '<path d="M12 3.5l1.8 5.2 5.2 1.8-5.2 1.8L12 17.5l-1.8-5.2L5 10.5l5.2-1.8zM19 16.5v4M17 18.5h4"/>',
    server: '<rect x="3.5" y="4" width="17" height="6" rx="1.5"/><rect x="3.5" y="14" width="17" height="6" rx="1.5"/><path d="M7 7h.01M7 17h.01"/>',
    calcheck: '<rect x="3.5" y="5" width="17" height="15" rx="2"/><path d="M3.5 10h17M8 3v4M16 3v4M9 15l2 2 4-4"/>'
  };

  /* ------------------------------------------------------------------ versions: each stores only its settings; engines load once.
     To add a version: add an entry here (newest first) and a builder in BUILDERS. Earlier versions stay in the page, live. */
  const VERSIONS = [
    { id: 'v6', label: 'v6 · Coin designs', note: 'Eight coin designs with real depth, drawn live in 3D: Dial (gauge bezel), Heatsink, Guilloché, Silicon, Turbine and Rack. Diamond and Ruby are set into the Dial coin like a lens element, not loose. Ideas to pick from: nothing in the app changes until you pick.' },
    { id: 'v5', label: 'v5 · Aging per plugin v0.6.0', note: 'Aging follows the achievements plugin v0.6.0: each medal keeps [start, dust left, web cleared] per tier. The dust clock runs from the day the tier was earned; a rub clears the cobweb (it grows back from that day) and takes 30% of the dust. Spotless starts the clocks again, with a bonus. Your saved look (366 days) is the default; every medal has its own web. Tests cover the dust curve at 10 to 365 days, cobweb from day 20 to 350, rubs, new tiers, the bubble, and the same seed in the page and the app. Smooth: the bubble mark always faces you (only the glass turns); dust and cobweb are drawn once, not every frame; the badge grid keeps each frame under budget (badges take turns); the 3D view keeps one GL context.' },
    { id: 'v4', label: 'v4 · Shaders verified on Windows', note: 'The HLSL medal, stone and bubble shaders compiled and rendered on Windows (CI, 22 Nov 2026). No fallback to flat medals. In the full view every medal is a real 3D object: drag it. Medals age with real time (1 hour = 1 day): grainy dust that piles up in the hollows, and cobwebs that grow thread by thread after 20 days. Every medal gets its own web. Polish them in the full view.' },
    { id: 'v3', label: 'v3 · Built into the app', note: 'Core, observer, window, pop-ups and full view are in the app (v2.13.0). Build status as it stood before the Windows run.' },
    { id: 'v2', label: 'v2 · Uptime Medals picked', note: 'The theme is picked; the build status as it stood then.' },
    { id: 'v1', label: 'v1 · Theme options', note: 'Earlier options: the three themes shown before the pick (Case Badges, Uptime Medals, Hall of Fame Plaques).' }
  ];

  Object.assign(RMC, { $, $$, esc, fmt, clamp, reduced, store, TIER, KIND, TVAR, THEX, XP_TIER, LV_TITLES, RANKS, xpNext, titleOf, rankOf, THEME, ICONS, VERSIONS });

  /* ------------------------------------------------------------------ v6: coin designs (live 3D; drag the big one) */
  const CMET = { gold: [1, .77, .33], plat: [.72, .93, .91], silver: [.93, .94, .97], rose: [.98, .62, .52], bronze: [.86, .52, .30], copper: [.95, .55, .38] };
  const COINS = [
    { id: 'dia', name: 'Dial · Diamond', tag: 'Diamond tier', design: 0, stone: 1, metal: 'plat', icon: 'cpu', title: 'Night Watch', sub: 'Diamond · 2026', ring: 'RESOURCE MONITOR  ·  99.99%  ·  DIAMOND MEDAL', text: 'The Diamond is set into the coin like a lens element in its barrel: a closed cup behind it, a black barrel round it, the scale and the name on the bezel. Turn it over: the cup bulges out the back like a lens.' },
    { id: 'ruby', name: 'Dial · Ruby', tag: 'Ruby tier', design: 0, stone: 2, metal: 'rose', icon: 'cpu', title: 'Night Watch', sub: 'Ruby · 2026', ring: 'RESOURCE MONITOR  ·  99.999%  ·  RUBY MEDAL', text: 'The same barrel in rose gold, holding the Ruby. Light that goes through the stone bounces off the polished cup and comes back red.' },
    { id: 'dial', name: 'Dial', tag: 'gauge bezel', design: 0, stone: 0, metal: 'gold', icon: 'gauge', title: 'Night Watch', sub: 'Gold · 2026', ring: 'RESOURCE MONITOR  ·  99.9%  ·  GOLD MEDAL', text: 'A gauge you can hold: knurled grip ring with beads, a sloped bezel with a 60-step scale, a black barrel, and a turned dome with the icon engraved.' },
    { id: 'heat', name: 'Heatsink', tag: 'CPU cooler', design: 1, metal: 'silver', metal2: 'copper', icon: 'cpu', title: 'Night Watch', sub: 'Silver · 2026', text: '40 machined fins round a copper slug, like the cooler on a CPU. The icon is engraved in the copper.' },
    { id: 'guil', name: 'Guilloché', tag: 'engine-turned', design: 2, metal: 'gold', icon: 'activity', title: 'Night Watch', sub: 'Gold · 2026', text: 'A waved rosette cut into the field like a watch dial, under a stepped rim, with a beaded cartouche that carries the icon.' },
    { id: 'sil', name: 'Silicon', tag: 'CPU die', design: 3, metal: 'plat', metal2: 'gold', icon: 'cpu', title: 'Night Watch', sub: 'Platinum · 2026', text: 'A black die with the icon etched in bright metal, gold pins on all four sides, and gold traces running out to pads at the rim.' },
    { id: 'turb', name: 'Turbine', tag: 'case fan', design: 4, metal: 'gold', icon: 'snow', title: 'Night Watch', sub: 'Gold · 2026', text: 'Nine curved blades turn slowly in a black well, round a turned hub with the icon. The only coin that moves on its own.' },
    { id: 'rack', name: 'Rack', tag: '1U faceplate', design: 5, metal: 'bronze', icon: 'server', title: 'Night Watch', sub: 'Bronze · 2026', ring: 'RACK 01  ·  WATCH', ringR: .82, ringTop: true, text: 'Terraces like a server faceplate, four rack screws, and the rack’s name on the middle step.' }
  ];
  const coinOpt = (c) => ({ design: c.design, stone: c.stone || 0, metal: CMET[c.metal], metal2: CMET[c.metal2 || 'copper'], icon: c.icon, title: c.title, sub: c.sub,
    ring: c.ring ? [{ text: c.ring, r: c.ringR || .775, px: c.ringTop ? 34 : 30, bottom: !c.ringTop }] : null });
  const CN = { sel: 0, on: false, raf: 0, yaw: 28, pitch: -16, drag: null, vy: 0, t0: 0, c: null };
  function buildV6() {
    const cards = COINS.map((c, i) => `<button type="button" class="cn-card" data-coin="${i}" aria-pressed="${i === 0}"><canvas width="320" height="320" aria-hidden="true"></canvas><b>${esc(c.name)}</b><span>${esc(c.tag)}</span></button>`).join('');
    return `<section class="card"><div class="sec-h"><div class="eyebrow">v6 · Coin designs</div><h2>Coins with depth, stones set in metal</h2>
      <p>Eight ideas, each a real 3D object with its own relief, drawn live. Diamond and Ruby are no longer loose stones: they sit in the Dial coin like a lens element in its barrel, as in your screenshot. Drag the big one to turn it over. Pick one design for every medal, or one design per tier.</p></div>
      <div class="cn"><div class="cn-stage"><canvas class="cn-c" width="792" height="792" aria-label="Selected coin, drag to turn"></canvas><div class="cap">drag to turn · double-click to reset</div></div>
      <div class="cn-info"><h3 class="cn-name"></h3><p class="cn-text"></p><div class="cn-grid" role="group" aria-label="Coin designs">${cards}</div></div></div></section>`;
  }
  function coinPick(i) {
    CN.sel = i; const c = COINS[i];
    $$('.cn-card').forEach((b) => b.setAttribute('aria-pressed', String(+b.dataset.coin === i)));
    const n = $('.cn-name'), t = $('.cn-text'); if (n) n.textContent = c.name; if (t) t.textContent = c.text;
    coinStart();
  }
  function coinTick(ts) {
    if (!CN.on) return;
    const s = (ts - CN.t0) / 1000;
    if (!CN.drag) { CN.yaw += CN.vy; CN.vy *= .94; }
    const sw = reduced() || CN.drag ? 0 : 5;
    YTPM3D.rot(CN.yaw + sw * Math.sin(s * .4), CN.pitch + sw * .4 * Math.sin(s * .29), 0);
    CN.raf = requestAnimationFrame(coinTick);
  }
  function coinStart() {
    const c = $('.cn-c'); if (!c || !window.YTPM3D) return;
    const r = c.getBoundingClientRect(), px = Math.round(Math.min(r.width || 396, 396) * Math.min(window.devicePixelRatio || 1, 2));
    if (px > 0 && c.width !== px) { c.width = px; c.height = px; }
    coinStop();
    CN.c = c; CN.on = YTPM3D.start(c, 'coin', reduced(), coinOpt(COINS[CN.sel]));
    if (CN.on) { CN.t0 = performance.now(); CN.raf = requestAnimationFrame(coinTick); }
  }
  function coinStop() { if (!CN.on) return; CN.on = false; cancelAnimationFrame(CN.raf); CN.raf = 0; try { if (YTPM3D.canvas() === CN.c) YTPM3D.stop(); } catch (e) { /* ignore */ } }
  function coinBind(p) {
    const c = $('.cn-c', p); if (!c || c.dataset.bound) return; c.dataset.bound = '1';
    $$('.cn-card', p).forEach((b) => b.addEventListener('click', () => coinPick(+b.dataset.coin)));
    c.addEventListener('pointerdown', (ev) => { if (!CN.on || YTPM3D.canvas() !== c) coinStart(); CN.drag = { x: ev.clientX, y: ev.clientY, lx: ev.clientX, t: performance.now() }; c.setPointerCapture(ev.pointerId); });
    c.addEventListener('pointermove', (ev) => { if (!CN.drag) return; const dx = ev.clientX - CN.drag.lx; CN.yaw += dx * .5; CN.pitch = Math.max(-70, Math.min(70, CN.pitch + (ev.clientY - CN.drag.y) * -.4)); CN.drag.y = ev.clientY; CN.vy = dx * .5; CN.drag.lx = ev.clientX; });
    const up = () => { CN.drag = null; };
    c.addEventListener('pointerup', up); c.addEventListener('pointercancel', up);
    c.addEventListener('dblclick', () => { CN.yaw = 28; CN.pitch = -16; CN.vy = 0; });
    const n = $('.cn-name', p), t = $('.cn-text', p); n.textContent = COINS[CN.sel].name; t.textContent = COINS[CN.sel].text;
    // the cards: one still each, drawn one after another after the big one starts (no long block)
    // (each waits for its own program; one card per idle slot, only once it is on screen, sized to how it is shown)
    const list = $$('.cn-card canvas', p), queue = [];
    let busy = false;
    const pump = () => {
      if (busy || !queue.length) return;
      busy = true;
      const c = queue.shift(), k = +c.closest('.cn-card').dataset.coin;
      const px = Math.max(64, Math.round((c.getBoundingClientRect().width || 160) * Math.min(window.devicePixelRatio || 1, 2)));
      if (c.width !== px) { c.width = px; c.height = px; }
      YTPM3D.snap(c, coinOpt(COINS[k]), 28, -16).then(() => { busy = false; YTPMGL.idle(pump, 300); });
    };
    const io = 'IntersectionObserver' in window ? new IntersectionObserver((es) => es.forEach((e) => { if (e.isIntersecting) { io.unobserve(e.target); queue.push(e.target); YTPMGL.idle(pump, 300); } })) : null;
    list.forEach((c) => { if (io) io.observe(c); else queue.push(c); });
    if (!io) YTPMGL.idle(pump, 300);
  }

  /* ------------------------------------------------------------------ versions bar + panels */
  const old = (id) => () => RMC.ver.build[id]();
  const BUILDERS = { v6: buildV6, v5: old('v5'), v4: old('v4'), v3: old('v3'), v2: old('v2'), v1: old('v1') };
  const panelsEl = $('#panels'), tabsEl = $('#vtabs');
  let cur = null;
  RMC.cur = () => cur;
  function renderTabs() {
    tabsEl.innerHTML = VERSIONS.map((v, i) => `<button class="vtab" role="tab" type="button" id="vt-${v.id}" data-v="${v.id}" aria-controls="p-${v.id}" aria-selected="false" tabindex="-1">${esc(v.label)}${i === 0 ? '<span class="cur">current</span>' : ''}</button>`).join('');
    panelsEl.innerHTML = VERSIONS.map((v) => `<div class="panel" role="tabpanel" id="p-${v.id}" aria-labelledby="vt-${v.id}" hidden></div>`).join('');
  }
  /* Old version tabs (v1-v5) live in js/versions.js with the badge engines (js/gem.js, js/bubble.js): fetched the first time one opens. */
  const loaded = {};
  function loadScript(f) {
    if (loaded[f]) return loaded[f];
    return (loaded[f] = new Promise((res, rej) => { const el = document.createElement('script'); el.src = f; el.async = false; el.onload = () => res(); el.onerror = () => rej(new Error('could not load ' + f)); document.head.appendChild(el); }));
  }
  RMC.loadScript = loadScript;
  const engines = () => Promise.all(['js/gem.js', 'js/bubble.js'].map(loadScript));
  const oldTabs = () => Promise.all(['js/gem.js', 'js/bubble.js', 'js/versions.js'].map(loadScript));
  RMC.engines = engines;
  function show(id, focusTab) {
    if (!BUILDERS[id]) id = VERSIONS[0].id;
    cur = id;
    VERSIONS.forEach((v) => {
      const sel = v.id === id, t = $('#vt-' + v.id), p = $('#p-' + v.id);
      t.setAttribute('aria-selected', sel ? 'true' : 'false'); t.tabIndex = sel ? 0 : -1; p.hidden = !sel;
    });
    const p = $('#p-' + id);
    const open = () => {
      if (cur !== id) return;
      if (!p.dataset.built) {
        p.innerHTML = BUILDERS[id]() || ''; p.dataset.built = '1';
        if (id !== 'v6') { RMC.ver.mountIn(p); RMC.ver.bindPanel(p); }
      }
      if (RMC.ver) RMC.ver.onShow(id);
      if (id === 'v6') { coinBind(p); requestAnimationFrame(coinStart); }
    };
    if (id !== 'v6') coinStop();
    if (id === 'v6' || RMC.ver) open();
    else {
      if (!p.dataset.built) p.innerHTML = '<p class="loading-tab">Loading this version…</p>';
      oldTabs().then(open, (e) => { if (!p.dataset.built) p.innerHTML = '<p class="loading-tab">' + esc(e.message) + '</p>'; });
    }
    $('#vnote').textContent = VERSIONS.find((v) => v.id === id).note;
    if (focusTab) $('#vt-' + id).focus();
    store.set('rm-ach-version', id);
    try { history.replaceState(null, '', '#' + id); } catch (e) { /* sandboxed frame */ }
  }
  tabsEl.addEventListener('click', (ev) => { const b = ev.target.closest('.vtab'); if (b) show(b.dataset.v); });
  tabsEl.addEventListener('keydown', (ev) => {
    const ids = VERSIONS.map((v) => v.id), i = ids.indexOf(cur);
    if (ev.key === 'ArrowRight' || ev.key === 'ArrowDown') { ev.preventDefault(); show(ids[(i + 1) % ids.length], true); }
    else if (ev.key === 'ArrowLeft' || ev.key === 'ArrowUp') { ev.preventDefault(); show(ids[(i - 1 + ids.length) % ids.length], true); }
    else if (ev.key === 'Home') { ev.preventDefault(); show(ids[0], true); }
    else if (ev.key === 'End') { ev.preventDefault(); show(ids[ids.length - 1], true); }
  });

  function tnVbar() { const w = $('.vbar-wrap'); if (w) document.documentElement.style.setProperty('--vbar-h', w.offsetHeight + 'px'); }
  window.addEventListener('resize', tnVbar);
  window.YTPMIconSrc = (n) => ICONS[n] || '';
  RMC.show = show;
  /* Warm-up, a few seconds after load: the programs other views need compile in the background (the full view's 3D
     programs, then the badge engines and their programs), so opening them later does not wait. */
  // Order: the badge grid's programs first (the next tab people open), then the full view's.
  const warmUp = () => setTimeout(() => YTPMGL.idle(() => {
    engines().then(() => YTPMGem.warm()).then(() => YTPMBubble.warm()).then(() => YTPM3D.warm()).catch(() => {});
  }, 2000), 3000);
  if (document.readyState === 'complete') warmUp(); else window.addEventListener('load', warmUp);
  /* ------------------------------------------------------------------ start */
  renderTabs();
  let start = VERSIONS[0].id;
  try { const h = (location.hash || '').slice(1); if (BUILDERS[h]) start = h; } catch (e) { /* ignore */ }
  show(start);
})();
