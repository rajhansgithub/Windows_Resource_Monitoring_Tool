/* Versions v1-v5 of the achievements preview: catalog, badge grid, full view, tuners, moments. Loaded the first time one of
   those tabs opens (after js/gem.js and js/bubble.js), by js/app.js. */
(function () {
  'use strict';
  const RMC = window.__RMC;
  const { $, $$, esc, fmt, clamp, reduced, store, TIER, KIND, TVAR, THEX, XP_TIER, LV_TITLES, RANKS, xpNext, titleOf, rankOf, THEME, ICONS, VERSIONS } = RMC;
  /* ------------------------------------------------------------------ canvases */
  function cv(o) {
    if (o.tier === 7 || o.kind === 'emu') return `<canvas class="ach-bubble" data-size="${o.size}" aria-hidden="true"></canvas>`;
    const kind = o.kind || KIND[o.tier - 1];
    return `<canvas class="ach-gemgl" data-kind="${kind}" data-shape="${o.shape || 'round'}" data-icon="${o.icon || ''}" data-size="${o.size}"${o.wid ? ` data-wid="${esc(o.wid)}"` : ''}${o.dpr ? ` data-dpr="${o.dpr}"` : ''}${o.pre ? ` data-pre="${esc(o.pre)}" data-title="${esc(o.title)}" data-line="${esc(o.line)}" data-sub="${esc(o.sub)}"` : ''} aria-hidden="true"></canvas>`;
  }
  function mountIn(el) {
    try { YTPMGem.mount(el); } catch (e) { /* no shader: parent gets .is-nogl */ }
    try { YTPMBubble.mount(el); } catch (e) { /* ignore */ }
    applyWear(el);
  }

  /* ------------------------------------------------------------------ age (achievements plugin v4): 1 real hour = 1 day on the shelf.
     Per medal: [start, dust left, web cleared] (the app keeps the same per "badgeId:tier" in achievements.json, key "ag").
     Dust shown = dust left x the dust curve from start (full at a year). A rub clears the cobweb (it regrows from that day) and
     keeps 70% of the dust; the dust clock keeps running. Spotless: the clocks start again, + bonus. The bubble never ages. */
  const HOUR_MS = 3600e3, WEAR_KEY = 'rm-wear-v2';
  const WEAR = (() => { try { const o = JSON.parse(store.get(WEAR_KEY) || '{}'); return o && typeof o === 'object' ? o : {}; } catch (e) { return {}; } })();
  if (!WEAR._) WEAR._ = { shine: 0 };
  const saveWear = () => store.set(WEAR_KEY, JSON.stringify(WEAR));
  const hid = (str) => { let h = 2166136261; for (let i = 0; i < str.length; i++) { h ^= str.charCodeAt(i); h = Math.imul(h, 16777619); } return (h >>> 0) / 4294967296; };
  function wearRec(id) {
    let r = WEAR[id];
    if (!r) { const t = Date.now() - (4 + 36 * hid(id)) * HOUR_MS; r = WEAR[id] = { a: t, l: 1, w: t }; saveWear(); }   // DEMO: earned 4 to 40 days ago
    return r;
  }
  /** This medal's own seed for its cobweb and dust (0..1). FNV-1a of the id, then a murmur3 finish so ids one letter apart
      still get very different webs. The app has the same function (ui.rs web_seed). */
  function webSeed(id) {
    let h = 2166136261; for (let i = 0; i < id.length; i++) { h ^= id.charCodeAt(i); h = Math.imul(h, 16777619); }
    h ^= h >>> 16; h = Math.imul(h, 0x85ebca6b); h ^= h >>> 13; h = Math.imul(h, 0xc2b2ae35); h ^= h >>> 16;
    return (h >>> 0) / 4294967296;
  }
  const dustCurve = (days) => Math.min(1, YTPMAge.dustAmt * (1 - Math.exp(-Math.max(0, days) / YTPMAge.dustDays)));
  const webCurve = (days) => days < YTPMAge.webStart ? 0 : Math.min(1, Math.pow((days - YTPMAge.webStart) / YTPMAge.webSpan, .8));
  function wearOf(id) {
    const r = wearRec(id), now = Date.now(), ds = Math.max(0, (now - r.a) / HOUR_MS), dw = Math.max(0, (now - Math.max(r.w, r.a)) / HOUR_MS);
    const d = r.l * dustCurve(ds), s = 0, w = webCurve(dw);   // dust and cobwebs only: no scratches (the owner 11-22)
    return { d, s, sd: webSeed(id), w, days: dw, n: r.w > r.a ? 1 : 0, cond: Math.round(100 * (1 - (.7 * d + .3 * w))), clean: d < .02 && w < .01 };
  }
  /** One rub: clears the cobweb (it regrows from now), keeps 70% of the dust (the tuner's "dust off per rub"). Spotless: start again. */
  function polish(id) {
    const w = wearOf(id), r = wearRec(id), now = Date.now(), keep = 1 - YTPMAge.rubDust;
    r.l *= keep; r.w = now;
    if (r.l < .05 || w.d * keep < .02) { r.a = now; r.l = 1; r.w = now; }
    saveWear();
    return wearOf(id);
  }
  function ageDay(id) { const r = wearRec(id); r.a -= HOUR_MS; r.w -= HOUR_MS; saveWear(); return wearOf(id); }   // test: one more day on the shelf
  function applyWear(scope) {
    $$('canvas.ach-gemgl[data-wid]', scope || document).forEach((c) => { const w = wearOf(c.dataset.wid); try { YTPMGem.set(c, { wear: [w.d, w.sd, w.w] }); } catch (e) { /* ignore */ } });
  }
  const dustyTag = (id) => { const w = wearOf(id); return w.w > .3 ? '<span class="tg dusty" title="Needs a polish">COBWEBS</span>' : w.cond < 70 ? '<span class="tg dusty" title="Needs a polish">DUSTY</span>' : ''; };
  setInterval(() => { if (!document.hidden) applyWear(document); }, 60e3);
  try { YTPMGem.icons((n) => ICONS[n] || ''); } catch (e) { /* ignore */ }

  /* ------------------------------------------------------------------ BUILD STATUS (the lead updates statuses here: 'done' | 'wip' | 'missing') */
  const BUILD_STATUS = [
    { name: 'Theme picked', st: 'done', note: 'Uptime Medals (round struck medal + 1U rack plates). Bubble keeps only the pulse line.' },
    { name: 'Catalog spec', st: 'done', note: '32 badges: 13 ladders, 3 rack sets, 5 start, 4 mastery, 6 accidents, 4 secrets. docs/ACHIEVEMENTS.md.' },
    { name: 'Core logic + storage + merge', st: 'done', note: 'src/achievements/core.rs, store.rs, json.rs. %APPDATA% mirrored to %LOCALAPPDATA%. Runs in Windows CI.' },
    { name: 'Observer hooks', st: 'done', note: 'Active minutes on the existing sample tick, alerts fired/cleared, End task, update installed, billing cycle. No new timers.' },
    { name: 'Native shaders (HLSL / D3D11 port)', st: 'done', note: 'Verified on Windows: vs_5_0 + both ps_5_0 compile with D3DCompile, all 7 tiers render with shading and motion. No flat-medal fallback.' },
    { name: 'Badge tab window', st: 'done', note: 'Opened on Windows (ResourceMonitor.exe --achievements): header, rack plates and every tier drawn by the GPU path.' },
    { name: 'Moments + level-up', st: 'wip', note: 'Pop-up near the tray, +XP pill, 6 s bar; level-up with 18 rays. Built, not yet triggered in the Windows run.' },
    { name: 'Full view + 3D award + spec-sheet back', st: 'wip', note: 'Tilt, idle sway, flip to the spec sheet, arrows. Simpler than here: no blur, no edge thickness, no glare.' },
    { name: 'Emulsion gate', st: 'done', note: 'Level 30, 180 days in 6 months, 3 ladders at Ruby, 2,000 hours + 5 TB. Sticky once open.' },
    { name: 'Settings (pop-ups, level-up, bubble sliders)', st: 'wip', note: 'Achievements on/off is in Settings. Separate pop-up and level-up toggles and bubble sliders are still missing.' },
    { name: 'Tests', st: 'done', note: '57 tests in Windows CI, now with the shader pixel check (coverage, shading, premultiplied alpha, motion) and an app run that opens the window.' },
    { name: 'docs/ACHIEVEMENTS.md', st: 'done', note: 'Mode, theme and why, scoring, catalog, gate numbers, storage.' }
  ];
  const BUILD_STATUS_V3 = [
    { name: 'Theme picked', st: 'done', note: 'Uptime Medals (round struck medal + 1U rack plates). Bubble keeps only the pulse line.' },
    { name: 'Catalog spec', st: 'done', note: '32 badges: 13 ladders, 3 rack sets, 5 start, 4 mastery, 6 accidents, 4 secrets. docs/ACHIEVEMENTS.md.' },
    { name: 'Core logic + storage + merge', st: 'done', note: 'src/achievements/core.rs, store.rs, json.rs. %APPDATA% mirrored to %LOCALAPPDATA%. Runs in Windows CI.' },
    { name: 'Observer hooks', st: 'done', note: 'Active minutes on the existing sample tick, alerts fired/cleared, End task, update installed, billing cycle. No new timers.' },
    { name: 'Native shaders (HLSL / D3D11 port)', st: 'wip', note: 'Written; compiled by Windows at run time only. Not yet seen on a real PC. Flat medal fallback if the GPU path fails.' },
    { name: 'Badge tab window', st: 'wip', note: 'Built (tray menu: Achievements…). Header, rack plates, gate card, category cards, rows, pips. Not yet seen on a real PC.' },
    { name: 'Moments + level-up', st: 'wip', note: 'Pop-up near the tray, +XP pill, 6 s bar; level-up with 18 rays. One moment at a time. Not yet seen on a real PC.' },
    { name: 'Full view + 3D award + spec-sheet back', st: 'wip', note: 'Tilt, idle sway, flip to the spec sheet, arrows. Simpler than here: no blur, no edge thickness, no glare.' },
    { name: 'Emulsion gate', st: 'done', note: 'Level 30, 180 days in 6 months, 3 ladders at Ruby, 2,000 hours + 5 TB. Sticky once open.' },
    { name: 'Settings (pop-ups, level-up, bubble sliders)', st: 'wip', note: 'Achievements on/off is in Settings. Separate pop-up and level-up toggles and bubble sliders are still missing.' },
    { name: 'Tests', st: 'wip', note: '56 unit tests run in Windows CI (core, store, JSON, observer). Shader pixel checks still missing.' },
    { name: 'docs/ACHIEVEMENTS.md', st: 'done', note: 'Mode, theme and why, scoring, catalog, gate numbers, storage.' }
  ];
  const BUILD_STATUS_V2 = [
    { name: 'Theme picked', st: 'done', note: 'Uptime Medals (round struck medal + 1U rack plates). Chosen over Case Badges and Hall of Fame Plaques.' },
    { name: 'Catalog spec', st: 'done', note: '13 ladders, 3 sets, singles, secrets: written in docs/ACHIEVEMENTS.md.' },
    { name: 'Core logic + storage + merge', st: 'wip', note: 'apply / merge / normalize / levelOf / rankOf / tierFor, JSON in %APPDATA% mirrored to %LOCALAPPDATA%.' },
    { name: 'Observer hooks', st: 'missing', note: 'Feed counters from samples, alerts, End task and updates. In-process, never a new service.' },
    { name: 'Native shaders (HLSL / D3D11 port)', st: 'wip', note: 'Medal, brilliant (Diamond, Ruby) and the Emulsion bubble, ported line by line from the reference.' },
    { name: 'Badge tab window', st: 'missing', note: 'Category cards, rows with live medal, pips, progress, rank pill, Emulsion gate card.' },
    { name: 'Moments + level-up', st: 'missing', note: 'One pop-up per batch, 6 s timer, level-up burst with 18 rays.' },
    { name: 'Full view + 3D award + spec-sheet back', st: 'missing', note: 'Tilt, sway, flip to the spec sheet, arrows through earned medals.' },
    { name: 'Emulsion gate', st: 'wip', note: 'In progress together with the core: level 30, 180 days in 6 months, 3 ladders at Ruby, 2,000 hours + 5 TB.' },
    { name: 'Settings (pop-ups, level-up, bubble sliders)', st: 'missing', note: 'Pop-ups on/off, level-up on/off, bubble sliders with live preview and Reset.' },
    { name: 'Tests', st: 'wip', note: 'Core unit tests started; migration fixtures, gate (each condition missing) and shader pixel checks still to do.' },
    { name: 'docs/ACHIEVEMENTS.md', st: 'done', note: 'Mode, theme and why, scoring, catalog, gate numbers, storage.' }
  ];
  const ST_LABEL = { done: 'Done', wip: 'In progress', missing: 'Missing' };

  /* ------------------------------------------------------------------ catalog (docs/ACHIEVEMENTS.md) with a clearly marked DEMO progress state (n) */
  const LADDERS = [
    { id: 'watch', name: 'Watchkeeper', icon: 'clock', what: 'active hours watched', th: [5, 50, 250, 1000, 2500, 6000, 12000], n: 1120 },
    { id: 'heavy', name: 'Heavy Lifting', icon: 'flame', what: 'hours with CPU at 80% or more', th: [1, 10, 50, 200, 600, 1500, 3000], n: 230 },
    { id: 'render', name: 'Render Farm', icon: 'gpu', what: 'hours with GPU at 50% or more', th: [1, 10, 50, 200, 600, 1500, 3000], n: 61 },
    { id: 'mover', name: 'Data Mover', icon: 'disk', what: 'GB read and written to disk', th: [50, 500, 2000, 10000, 40000, 100000, 250000], n: 2400 },
    { id: 'bandwidth', name: 'Bandwidth', icon: 'globe', what: 'GB of network traffic tracked', th: [10, 100, 500, 2000, 8000, 20000, 50000], n: 640 },
    { id: 'down', name: 'Downloader', icon: 'down', what: 'GB downloaded', th: [10, 100, 400, 1500, 6000, 15000, 40000], n: 410 },
    { id: 'up', name: 'Uploader', icon: 'up', what: 'GB uploaded', th: [1, 10, 50, 250, 1000, 3000, 8000], n: 0.6 },
    { id: 'gameon', name: 'Game On', icon: 'pad', what: 'hours of full-screen apps', th: [5, 50, 200, 600, 1500, 3500, 7000], n: 70 },
    { id: 'responder', name: 'First Responder', icon: 'bell', what: 'alerts handled (cleared within 30 min)', th: [1, 5, 25, 100, 300, 800, 2000], n: 7 },
    { id: 'taskmaster', name: 'Task Master', icon: 'xcircle', what: 'tasks ended from the panel', th: [1, 5, 25, 100, 300, 800, 2000], n: 3 },
    { id: 'regular', name: 'Regular', icon: 'calendar', what: 'days of real use', th: [3, 15, 50, 150, 365, 730, 1460], n: 212 },
    { id: 'ritual', name: 'Weekly Ritual', icon: 'repeat', what: 'weeks with 3+ days of real use', th: [2, 8, 26, 52, 104, 208, 416], n: 28 },
    { id: 'stillhere', name: 'Still Here', icon: 'hourglass', what: 'months with a day of real use', th: [2, 6, 12, 24, 36, 60, 120], n: 7 }
  ];
  const SINGLES = [
    { id: 'power_on', cat: 'start', name: 'Power On', icon: 'power', xp: 25, line: 'First active hour watched.', ok: 1 },
    { id: 'second_screen', cat: 'start', name: 'Second Screen', icon: 'screens', xp: 25, line: 'Widgets on 2+ taskbars for a full active hour.', ok: 1 },
    { id: 'up_to_date', cat: 'start', name: 'Up to Date', icon: 'refresh', xp: 25, line: 'An automatic update installed.', ok: 1 },
    { id: 'under_cap', cat: 'start', name: 'Under the Cap', icon: 'gauge', xp: 25, line: 'A whole billing cycle finished under your data cap.', ok: 0, p: [19, 30], pt: 'day 19 of 30' },
    { id: 'first_gb', cat: 'start', name: 'First Gigabyte', icon: 'box', xp: 25, line: '1 GB of traffic tracked.', ok: 1 },
    { id: 'marathon', cat: 'mastery', name: 'Marathon', icon: 'flag', xp: 250, line: '24 hours of continuous active watching in one session.', ok: 0, p: [11, 24], pt: '11 / 24 hours' },
    { id: 'cool_week', cat: 'mastery', name: 'Cool Customer', icon: 'snow', xp: 250, line: '7 days of real use in a row with no alert.', ok: 1 },
    { id: 'tb_month', cat: 'mastery', name: 'Terabyte Club', icon: 'stack', xp: 300, line: '1 TB tracked in one calendar month.', ok: 0, p: [420, 1000], pt: '420 / 1,000 GB this month' },
    { id: 'completionist', cat: 'mastery', name: 'Completionist', icon: 'star', xp: 500, line: 'Every badge you can earn in this build.', ok: 0, p: null },
    { id: 'running_hot', cat: 'acc', name: 'Running Hot', icon: 'thermo', xp: 10, line: 'GPU at 90 °C or more.', ok: 1 },
    { id: 'red_line', cat: 'acc', name: 'Red Line', icon: 'ram', xp: 10, line: 'Memory at 98% or more.', ok: 1 },
    { id: 'full_house', cat: 'acc', name: 'Full House', icon: 'disk', xp: 10, line: 'System drive 98% full.', ok: 0 },
    { id: 'gone_dark', cat: 'acc', name: 'Gone Dark', icon: 'nowifi', xp: 10, line: 'No network traffic for 10 active minutes.', ok: 1 },
    { id: 'access_denied', cat: 'acc', name: 'Access Denied', icon: 'shieldx', xp: 10, line: 'End task refused by Windows.', ok: 1 },
    { id: 'fumes', cat: 'acc', name: 'Running on Fumes', icon: 'battery', xp: 10, line: 'Battery at 5% or less, unplugged.', ok: 0 },
    { id: 'night_owl', cat: 'secret', name: 'Night Owl', icon: 'moon', xp: 25, line: 'Real use between 2 and 4 am.', ok: 1, secret: 1 },
    { id: 'one_year', cat: 'secret', name: 'One Year', icon: 'calcheck', xp: 25, line: '365 days since the first day of real use.', ok: 0, secret: 1 },
    { id: 'spring_clean', cat: 'secret', name: 'Spring Cleaning', icon: 'sparkle', xp: 25, line: 'System drive use drops 10 points within a day.', ok: 0, secret: 1 },
    { id: 'five_nines', cat: 'secret', name: 'Five Nines', icon: 'server', xp: 25, line: 'Windows uptime of 30 days seen.', ok: 0, secret: 1 }
  ];
  const SINGLE_TIER = { start: 1, mastery: 4, acc: 2, secret: 3 };   // single badges wear a fixed metal (no ladder)
  const SETS = [
    { id: 'watch', name: 'Rack 01 · Watch', members: ['watch', 'heavy', 'render', 'mover'] },
    { id: 'flow', name: 'Rack 02 · Flow', members: ['bandwidth', 'down', 'up', 'gameon'] },
    { id: 'care', name: 'Rack 03 · Care', members: ['responder', 'taskmaster', 'regular', 'ritual'] }
  ];
  const DEMO = { level: 12, xp: 612 };

  const LMAP = {}; LADDERS.forEach((L) => { LMAP[L.id] = L; });
  function ladderState(L) {   // the Emulsion gate is shut in the demo, so ladders stop at Ruby (6)
    let t = 0; for (let i = 0; i < 6; i++) if (L.n >= L.th[i]) t = i + 1;
    const next = L.th[t], prev = t ? L.th[t - 1] : 0;
    return { t, next, prev, frac: clamp((L.n - prev) / (next - prev), 0, 1) };
  }
  function setState(S) {
    const tiers = S.members.map((id) => ladderState(LMAP[id]).t);
    const rec = Math.min.apply(null, tiers), next = Math.min(rec + 1, 6);
    return { tiers, rec, next, cnt: tiers.filter((t) => t >= next).length };
  }
  function hash(str) { let h = 2166136261; for (let i = 0; i < str.length; i++) { h ^= str.charCodeAt(i); h = Math.imul(h, 16777619); } return h >>> 0; }
  function demoDate(id, i) {   // demo earn days: deterministic, never in the future
    const d = new Date(Date.UTC(2026, 2, 5) + (hash(id) % 15 + i * 34) * 86400000);
    return d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric', timeZone: 'UTC' });
  }
  const certNo = (id, t) => 'RM-' + ('0000' + (hash(id + ':' + t) % 65536).toString(16).toUpperCase()).slice(-4);

  /* entries: one per earned row (used by the full view) */
  const ENT = {}, ORDER = [];
  LADDERS.forEach((L) => {
    const s = ladderState(L);
    ENT[L.id] = { id: L.id, kind: 'ladder', title: L.name, line: `${fmt(L.n)} ${L.what}`, tier: s.t, icon: L.icon, L, s, earned: s.t > 0 };
  });
  SINGLES.forEach((B) => {
    ENT[B.id] = { id: B.id, kind: 'single', title: B.name, line: B.line, tier: SINGLE_TIER[B.cat], icon: B.icon, B, earned: !!B.ok };
  });
  const earnedCount = () => SINGLES.filter((b) => b.ok).length + LADDERS.filter((L) => ladderState(L).t > 0).length;
  const buildTotal = LADDERS.length + SINGLES.length;
  const CATS = [
    { id: 'ladders', name: 'Ladders', ids: LADDERS.map((x) => x.id), wide: 1, first: 5 },
    { id: 'start', name: 'Getting started', ids: SINGLES.filter((x) => x.cat === 'start').map((x) => x.id) },
    { id: 'mastery', name: 'Mastery', ids: SINGLES.filter((x) => x.cat === 'mastery').map((x) => x.id) },
    { id: 'acc', name: 'Happy accidents', ids: SINGLES.filter((x) => x.cat === 'acc').map((x) => x.id) },
    { id: 'secret', name: 'Secrets', ids: SINGLES.filter((x) => x.cat === 'secret').map((x) => x.id) }
  ];
  CATS.forEach((C) => C.ids.forEach((id) => { if (ENT[id].earned) ORDER.push(id); }));
  const done = SINGLES.filter((b) => b.ok && b.id !== 'completionist').length + LADDERS.filter((L) => ladderState(L).t > 0).length;
  const others = buildTotal - 1;
  SINGLES.find((b) => b.id === 'completionist').p = [done, others];
  SINGLES.find((b) => b.id === 'completionist').pt = `${done} / ${others} badges`;

  /* ------------------------------------------------------------------ rows */
  function pipsHtml(t) {
    let h = '<span class="pips" role="img" aria-label="' + t + ' of 7 tiers">';
    for (let i = 0; i < 7; i++) h += `<i style="${i < t ? 'background:' + TVAR[i] : ''}"></i>`;
    return h + '</span>';
  }
  function rowHtml(id) {
    const e = ENT[id];
    let title = e.title, line = e.line, bar = '', meta = '';
    if (e.kind === 'ladder') {
      const s = e.s;
      const nt = s.t < 6 ? `Next: ${TIER[s.t]} at ${fmt(s.next)}` : 'Emulsion: gate shut';
      bar = `<div class="rbar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${Math.round(s.frac * 100)}" aria-label="Progress to ${TIER[Math.min(s.t, 6)]}"><i style="width:${Math.round(s.frac * 100)}%;background:${TVAR[Math.min(s.t, 6)]}"></i></div>`;
      meta = `<div class="rmeta">${pipsHtml(s.t)}<span class="rnext">${nt}</span></div>`;
    } else {
      const B = e.B;
      if (B.secret && !B.ok) { title = '?'; line = 'A secret. Keep using Resource Monitor.'; }
      if (B.ok) meta = `<div class="rmeta"><span class="rnext">Earned ${demoDate(B.id, 0)}</span><span class="xpc">+${B.xp} XP</span></div>`;
      else if (B.p) {
        const f = clamp(B.p[0] / B.p[1], 0, 1);
        bar = `<div class="rbar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${Math.round(f * 100)}" aria-label="Progress"><i style="width:${Math.round(f * 100)}%;background:var(--accent)"></i></div>`;
        meta = `<div class="rmeta"><span class="rnext">${B.pt}</span><span class="xpc">${B.xp} XP</span></div>`;
      } else meta = `<div class="rmeta"><span class="rnext">${B.secret ? 'Locked' : 'Not yet'}</span><span class="xpc">${B.secret ? '? XP' : B.xp + ' XP'}</span></div>`;
    }
    const badge = e.earned ? cv({ tier: e.tier, icon: e.icon, size: 64, wid: e.id }) : `<div class="ph" aria-hidden="true">${e.kind === 'single' && e.B.secret ? '?' : ''}</div>`;
    const lab = e.earned ? `${title}, ${TIER[e.tier - 1]}, ${esc(line)}. Open full view` : `${title}, not earned yet`;
    return `<div class="row${e.earned ? ' earned' : ''}"${e.earned ? ` role="button" tabindex="0" data-e="${id}"` : ''} aria-label="${esc(lab)}">
      <div class="rb">${badge}</div>
      <div class="rt"><div class="rtitle">${esc(title)}${e.earned ? `<span class="tg">${TIER[e.tier - 1].toUpperCase()}</span>${e.tier < 7 ? dustyTag(e.id) : ''}` : ''}</div><div class="rline">${esc(line)}</div>${bar}${meta}</div></div>`;
  }

  /* ------------------------------------------------------------------ sections of v2 */
  function heroHtml() {
    const tiles = TIER.map((n, i) => `<button class="tile" type="button" data-tile="${i}" aria-label="${n} medal, open full view">${cv({ tier: i + 1, icon: 'clock', size: 76, wid: 'tier' + i })}<span>${n}</span><i style="background:${TVAR[i]}"></i></button>`).join('');
    return `<section class="card" aria-labelledby="h-hero"><div class="hero2"><div>
      <div class="eyebrow">Version 2 · Uptime Medals picked</div>
      <h1 id="h-hero" style="margin-top:6px">Resource Monitor Achievements</h1>
      <p class="lead">Data centres celebrate uptime. Here every award is a <b>struck medal</b> with a spun face and the icon engraved, and a full set of four fills a <b>1U rack plate</b> like the ones that hold real servers. Diamond and Ruby are live cut stones, Emulsion is the live soap bubble with the Resource Monitor pulse inside.</p>
      <div class="chips"><span>Free mode · no paid plan</span><span>Emulsion earned over about a year</span><span>7 tiers · 3 racks of 4</span><span>Metal label: Uptime</span></div>
      </div><div class="herobadge"><button type="button" data-tile="2" aria-label="Gold medal, open full view">${cv({ tier: 3, icon: 'clock', size: 280, wid: 'tier2' })}</button><div class="cap">Uptime · Gold medal · tap to turn it over</div></div></div>
      <div class="tiers" role="list" aria-label="All seven tiers">${tiles}</div></section>`;
  }
  function rackHtml() {
    const vent = '<i></i>'.repeat(16);
    return SETS.map((S) => {
      const st = setState(S);
      const wells = S.members.map((id, k) => {
        const e = ENT[id];
        const inner = e.tier > 0 ? cv({ tier: e.tier, icon: e.icon, size: 112, wid: id }) : '<div class="empty" aria-hidden="true">0</div>';
        return `<div class="well">${inner}<b>${esc(e.title)}</b><small>${e.tier > 0 ? TIER[e.tier - 1] : 'Not started'}</small></div>`;
      }).join('');
      const lead = st.rec > 0 ? `${TIER[st.rec - 1]} rack` : 'No rack certified yet';
      const pips = st.tiers.map((t) => `<i class="${t >= st.next ? 'on' : ''}"></i>`).join('');
      return `<article class="rack" aria-label="${esc(lead + ' ' + S.name)}">
        <div class="ear" aria-hidden="true"><i class="hole"></i><i class="hole"></i></div>
        <div class="rack-mid">
          <div class="rack-top"><span class="rack-name">${esc(S.name)}</span><span class="leds" aria-hidden="true"><i class="led"></i><i class="led ${st.rec > 0 ? '' : 'off'}"></i><i class="led amber"></i></span><span class="vents" aria-hidden="true">${vent}</span></div>
          <div class="wells">${wells}</div>
          <div class="nameplate"><span class="pt">${esc(lead)} · ${esc(S.name)} · Resource Monitor</span><span class="nx">Next: ${TIER[Math.min(st.next - 1, 5)]} ${st.cnt}/4 <span class="mp" aria-hidden="true">${pips}</span></span></div>
        </div>
        <div class="ear" aria-hidden="true"><i class="hole"></i><i class="hole"></i></div></article>`;
    }).join('');
  }
  function gateHtml() {
    const watch = LMAP.watch.n, bw = LMAP.bandwidth.n / 1000;
    const rubies = LADDERS.filter((L) => ladderState(L).t >= 6).length;
    const days = LMAP.regular.n, months = LMAP.stillhere.n;
    const conds = [
      { n: 'Level 30 or higher', vals: [[`Lv ${DEMO.level} / 30`, DEMO.level >= 30]] },
      { n: 'Time in the tool: 180 days of real use, in 6 different months', vals: [[`${fmt(days)} / 180 days`, days >= 180], [`${months} / 6 months`, months >= 6]] },
      { n: 'Depth: 3 ladders at Ruby', vals: [[`${rubies} / 3 ladders`, rubies >= 3]] },
      { n: 'Tool factor: 2,000 active hours watched and 5 TB of traffic tracked', vals: [[`${fmt(watch)} / 2,000 hours`, watch >= 2000], [`${fmt(bw)} / 5 TB`, bw >= 5]] }
    ];
    return `<section class="gate" aria-labelledby="h-gate"><h3 id="h-gate"><span class="emu" aria-hidden="true"></span>Emulsion gate <span class="demo">demo values</span></h3>
      <p class="s">Emulsion opens only when all four hold. It is checked after real events, never by a timer, never by idle time.</p>
      ${conds.map((c) => { const ok = c.vals.every((v) => v[1]); return `<div class="cond${ok ? ' met' : ''}"><span class="tk" aria-hidden="true">✓</span><b>${esc(c.n)}<span class="sr">${ok ? ' (met)' : ' (not met yet)'}</span></b><div class="vals">${c.vals.map((v) => `<span class="${v[1] ? 'met' : ''}">${v[1] ? '✓ ' : ''}${esc(v[0])}</span>`).join('')}</div></div>`; }).join('')}</section>`;
  }
  function tabHtml() {
    const lv = DEMO.level, rk = rankOf(lv), need = xpNext(lv), C = 2 * Math.PI * 28, frac = DEMO.xp / need;
    const cats = CATS.map((C) => {
      const first = C.first || C.ids.length;
      const got = C.ids.filter((id) => ENT[id].earned).length;
      return `<section class="cat${C.wide ? ' wide' : ''}" aria-labelledby="c-${C.id}" data-cat="${C.id}"><h3 id="c-${C.id}">${C.name}<small>${got} / ${C.ids.length}</small></h3>
        <div class="rows">${C.ids.slice(0, first).map(rowHtml).join('')}</div>
        ${C.ids.length > first ? `<button class="more" type="button" aria-expanded="false" data-more="${C.id}">Show all ${C.ids.length}</button>` : ''}</section>`;
    }).join('');
    return `<div class="win"><div class="win-bar"><b>Resource Monitor</b> <span>· Badges</span><span class="gl" aria-hidden="true">─ □ ✕</span></div>
      <div class="win-body"><nav class="snav" aria-label="Mock navigation"><div>Overview</div><div>Alerts</div><div class="on">Badges <span class="rank-pill" style="background:${TVAR[rk[2]]}">Lv ${lv}</span></div><div>Settings</div></nav>
      <div class="tabmain">
        <div class="lvhead"><div class="ring"><svg viewBox="0 0 64 64" aria-hidden="true"><circle cx="32" cy="32" r="28" fill="none" stroke="#262c35" stroke-width="6"/><circle cx="32" cy="32" r="28" fill="none" stroke="${THEX[rk[2]]}" stroke-width="6" stroke-linecap="round" stroke-dasharray="${(C * frac).toFixed(1)} ${C.toFixed(1)}"/></svg><b>${lv}</b></div>
          <div><div class="rk" style="color:${THEX[rk[2]]}">${rk[1]} · ${titleOf(lv)}</div><div class="xp">${DEMO.xp} / ${need} XP to level ${lv + 1}</div></div><span class="sp"></span><span class="rank-pill" style="background:${TVAR[rk[2]]}">${rk[1]} ${lv}</span></div>
        ${gateHtml()}
        <div class="cats">${cats}</div></div></div></div>`;
  }
  function statusHtml(list, sfx) {
    const BUILD_STATUS = list;
    const n = { done: 0, wip: 0, missing: 0 }; BUILD_STATUS.forEach((b) => { n[b.st]++; });
    const tot = BUILD_STATUS.length;
    return `<section class="card" aria-labelledby="h-bs${sfx}"><div class="sec-h"><div class="eyebrow">Build status</div><h2 id="h-bs${sfx}">Gaps for the Windows app</h2><p>Every part the achievements skill requires, and where it stands. Statuses live in one list (<code>BUILD_STATUS</code>) so they are easy to update.</p></div>
      <div class="bs-sum"><span style="color:var(--ok)">${n.done} done</span><span style="color:var(--warn)">${n.wip} in progress</span><span style="color:var(--bad)">${n.missing} missing</span><span style="color:var(--dim)">of ${tot}</span></div>
      <div class="bs-bar" aria-hidden="true"><i style="width:${n.done / tot * 100}%;background:var(--ok)"></i><i style="width:${n.wip / tot * 100}%;background:var(--warn)"></i><i style="width:${n.missing / tot * 100}%;background:var(--bad)"></i></div>
      <ul class="bs-list">${BUILD_STATUS.map((b) => `<li><b>${esc(b.name)}</b><span class="st ${b.st}">${ST_LABEL[b.st]}</span><span class="nt">${esc(b.note)}</span></li>`).join('')}</ul></section>`;
  }
  function buildV2(list, sfx) {
    list = list || BUILD_STATUS_V2; sfx = sfx || '';
    return `${heroHtml()}
      <section aria-labelledby="h-rack${sfx}"><div class="sec-h"><div class="eyebrow">Set reward</div><h2 id="h-rack${sfx}">1U rack plates</h2><p>All four medals of a rack at one tier certify it. A rack plate never shows a higher tier than the medals hold. <span class="demo">demo progress</span></p></div><div class="plates">${rackHtml()}</div></section>
      <section aria-labelledby="h-tab${sfx}"><div class="sec-h"><div class="eyebrow">Badge tab</div><h2 id="h-tab${sfx}">What the user sees</h2><p>Category cards, a live medal per row, progress, seven pips, and the next goal. Tap an earned row for the full view. <span class="demo">demo progress, not real data</span></p></div>${tabHtml()}</section>
      <section class="card" aria-labelledby="h-mo${sfx}"><div class="sec-h"><div class="eyebrow">Moments</div><h2 id="h-mo${sfx}">The pop-up and the level-up</h2><p>One pop-up per batch, closes after 6 seconds or on click, Esc, Enter or Space. A level-up with no badge gets a full-screen burst of 18 rays.</p></div>
        <div class="moments"><button class="btn pri" type="button" id="btnMoment">Play an unlock moment</button><button class="btn" type="button" id="btnLevel">Play the level-up burst</button></div></section>
      ${statusHtml(list, sfx)}
      <section class="card" aria-labelledby="h-th${sfx}"><div class="sec-h"><div class="eyebrow">Theme contract</div><h2 id="h-th${sfx}">Uptime Medals</h2><p>Chosen over Case Badges and Hall of Fame Plaques, which stay live under Earlier options. Reason: Resource Monitor users watch their machines the way operators watch servers, and uptime is the number they are proud of.</p></div><div class="theme-line">${esc(THEME)}</div>
        <div class="moments" style="margin-top:14px"><button class="btn" type="button" id="btnV1">Open earlier options (v1)</button></div></section>`;
  }
  function buildV3() { return buildV2(BUILD_STATUS_V3, '-3'); }

  /* ------------------------------------------------------------------ v4: what Windows actually drew (CI run 44973046644, 22 Nov 2026).
     Numbers are from the test achievements::gpu::tests::shaders_compile_and_every_material_renders (192 px tile, still frame). */
  const VERIFY = [
    { icon: 'clock', badge: 'Watchkeeper', cov: 73, sd: 61.1, rgb: [108, 77, 49] },
    { icon: 'flame', badge: 'Heavy Lifting', cov: 73, sd: 65.3, rgb: [113, 114, 116] },
    { icon: 'gpu', badge: 'Render Farm', cov: 73, sd: 64.0, rgb: [118, 101, 53] },
    { icon: 'disk', badge: 'Data Mover', cov: 73, sd: 65.2, rgb: [96, 113, 111] },
    { icon: 'globe', badge: 'Bandwidth', cov: 73, sd: 65.7, rgb: [97, 97, 99] },
    { icon: 'down', badge: 'Downloader', cov: 73, sd: 29.8, rgb: [141, 20, 35] },
    { icon: 'up', badge: 'Uploader', cov: 79, sd: 35.3, rgb: [27, 26, 28] }
  ];
  const RUN = 'https://github.com/rajhansgithub/Windows_Resource_Monitoring_Tool/actions/runs/44986531579';
  const PR = 'https://github.com/rajhansgithub/Windows_Resource_Monitoring_Tool/pull/26';
  function verifyHtml() {
    const tiles = VERIFY.map((v, i) => `<button class="vf" type="button" data-tile="${i}" aria-label="${TIER[i]} medal, verified on Windows, open full view">
      ${cv({ tier: i + 1, icon: v.icon, size: 112, wid: 'tier' + i })}<b style="color:${THEX[i]}">${TIER[i]}</b><small>${esc(v.badge)}</small><span class="ok">✓ rendered</span>
      <dl><dt>cover</dt><dd>${v.cov}%</dd><dt>shading</dt><dd>sd ${v.sd}</dd><dt>colour</dt><dd><i class="sw" style="background:rgb(${v.rgb.join(',')})" title="mean rgb ${v.rgb.join(' ')}"></i></dd></dl></button>`).join('');
    const checks = [
      ['D3DCompile accepts all three shaders', 'vs_5_0 vsmain, ps_5_0 main for the medal / brilliant and for the Emulsion bubble. d3dcompiler_47.dll from System32.'],
      ['Every tier renders real shading', 'Coverage, luminance spread, premultiplied alpha and colour checked per material. Gold reads warmer than Silver. A flat disc would fail.'],
      ['Every tier moves', 'Time 2.5 s with tilt (0.35, -0.25) draws a different frame from the still frame, for all 7.'],
      ['The app draws them, not the fallback', '<code>ResourceMonitor.exe --achievements</code> opened the window on the Windows runner. Seeded one badge per tier; diagnostics.log has no “achievements gpu” line, so the GPU path ran.'],
      ['Guarded from now on', 'CI fails if a shader stops compiling, if the window does not open, or if the app logs a GPU fallback.']
    ].map((c) => `<li><span class="tk" aria-hidden="true">✓</span><b>${c[0]}</b><span class="nt">${c[1]}</span></li>`).join('');
    return `<section class="card" aria-labelledby="h-vf"><div class="sec-h"><div class="eyebrow">Verified on Windows · 22 Nov 2026</div><h2 id="h-vf">The medal shaders work</h2>
      <p>Each medal below is live, drawn by the same shader the HLSL port was made from, with the numbers Windows measured on its own render of that tier. Tap one for the full view.</p></div>
      <div class="vf-grid" role="list" aria-label="Seven tiers, verified">${tiles}</div>
      <ul class="vf-checks">${checks}</ul>
      <div class="vf-links"><a href="${PR}" target="_blank" rel="noopener">PR #26</a><a href="${RUN}" target="_blank" rel="noopener">Windows CI run</a></div></section>`;
  }

  /* ------------------------------------------------------------------ age tuner (full view of a metal or stone): drives YTPMAge */
  const AG_SPEC = [
    ['Dust', [['dustAmt', 'Dust amount', 0, 2, 0.01], ['dustPatch', 'Patchiness', 0, 1, 0.01], ['dustUp', 'Top first (bottom lags)', 0, 1, 0.01], ['dull', 'Dust hides reflections', 0, 1, 0.01], ['grit', 'Grit specks', 0, 1.5, 0.01], ['lint', 'Lint fibres', 0, 1.5, 0.01], ['dustDays', 'Dust clock (days)', 10, 800, 1]]],
    ['Cobwebs', [['webAmt', 'Cobweb amount', 0, 2, 0.01], ['webThreads', 'How many threads', 0.3, 1.4, 0.01], ['webWidth', 'Thread thickness', 0.3, 3, 0.01], ['webGlint', 'Silk glint', 0, 1.5, 0.01], ['webSheet', 'Gauze sheets', 0, 1, 0.01], ['webDust', 'Dust on the silk', 0, 2, 0.01], ['webRainbow', 'Rainbow on silk', 0, .5, 0.01], ['webStart', 'Cobwebs start after (days)', 0, 40, 1], ['webSpan', 'Days to a full cobweb', 30, 400, 1]]],
    ['Polish', [['rubDust', 'Dust off per rub', 0.1, 0.9, 0.01]]],
    ['Motion', [['sway', 'Idle sway', 0, 1, 0.01], ['hover', 'Hover tilt', 0, 1, 0.01], ['flick', 'Flick speed', 0, 3, 0.01], ['friction', 'Flick slows down', 0.5, 8, 0.1]]]
  ];
  const AG_KEY = 'rm-age-look-v4', AG_DOC = 'tuning/age';   // v2: dust sliders changed meaning (top first, hides reflections)
  try { const ls = JSON.parse(store.get(AG_KEY) || 'null'); if (ls) for (const k in YTPMAgeDefaults) if (typeof ls[k] === 'number') YTPMAge[k] = ls[k]; } catch (e) { /* ignore */ }
  const AG = { sets: [], dirty: false, status: '' };
  const agStep = (k) => AG_SPEC.flatMap((g) => g[1]).find((x) => x[0] === k)[4];
  const agFmt = (k, v) => (agStep(k) >= 1 ? String(Math.round(v)) : (+v).toFixed(agStep(k) >= .1 ? 1 : 2));
  function agStatus(t) { AG.status = t; $$('.ag-status').forEach((x) => { x.textContent = t; }); }
  function agApply() {   // the look changed: redraw, and the wear clock may have moved
    const e = FV.list[FV.i];
    if (e && e.tier < 7) wearRefresh(e, wearOf(e.id)); else applyWear(document);
    try { YTPM3D.redraw(); } catch (err) { /* ignore */ }
  }
  function agSync(skip) {
    AG.sets = AG.sets.filter((set) => set.box.isConnected);
    for (const set of AG.sets) for (const k in set.inputs) { if (set.inputs[k].inp === skip) continue; set.inputs[k].inp.value = YTPMAge[k]; set.inputs[k].val.textContent = agFmt(k, YTPMAge[k]); }
  }
  function agDirty() { AG.dirty = true; store.set(AG_KEY, JSON.stringify(YTPMAge)); agStatus(TN.db ? 'Changed, not saved yet' : 'Changed. Kept in this browser.'); }
  async function agSave() {
    if (!TN.db) return;
    $$('.ag-save').forEach((b) => { b.disabled = true; }); agStatus('Saving…');
    try {
      const out = {}; for (const k in YTPMAgeDefaults) out[k] = +YTPMAge[k];
      out.savedAt = new Date().toISOString();
      await TN.db.doc(AG_DOC).set(out);
      AG.dirty = false; agStatus('Saved. Claude will use these settings.');
    } catch (e) { agStatus('Could not save (' + ((e && e.code) || 'error') + '). Changes stay in this browser.'); }
    $$('.ag-save').forEach((b) => { b.disabled = !TN.db; });
  }
  async function agLoad() {   // the saved look, once the page has its database
    if (!TN.db || AG.loaded) return; AG.loaded = true;
    try { const snap = await TN.db.doc(AG_DOC).get(); if (snap.exists && !AG.dirty) { const v = snap.data() || {}; for (const k in YTPMAgeDefaults) if (typeof v[k] === 'number') YTPMAge[k] = v[k]; agSync(); agApply(); agStatus('Showing your saved settings'); } } catch (e) { /* keep */ }
  }
  /** Side panel for metals and stones: this medal's age, then the look of dust and cobwebs, then motion. */
  function agSidePanel(cardEl, e) {
    const d = document.createElement('div'); d.className = 'fv-tune ag';
    const days = Math.round(wearOf(e.id).days);
    d.innerHTML = `<div class="mt" style="color:#d9b98a">Tune the age</div>
      <fieldset class="tn-group"><legend>This medal</legend><label class="tn-row"><span>Days on the shelf (try it)</span><output class="ag-days">${days}</output><input type="range" class="ag-age" min="0" max="400" step="1" value="${Math.min(400, days)}"></label></fieldset>
      <div class="tn-sliders"></div>
      <div class="tn-actions"><button type="button" class="tn-save ag-save"${TN.db ? '' : ' disabled'}>Save for Claude</button><button type="button" class="tn-reset ag-reset">Reset</button><span class="tn-status ag-status" role="status">${esc(AG.status || (TN.db ? 'Not changed yet' : 'Changes stay in this browser'))}</span></div>`;
    cardEl.appendChild(d);
    const age = $('.ag-age', d), out = $('.ag-days', d);
    age.addEventListener('input', () => {
      const n = +age.value, r = wearRec(e.id);
      r.a = r.w = Date.now() - n * HOUR_MS; r.l = 1; saveWear();
      out.textContent = n; wearRefresh(e, wearOf(e.id));
    });
    const set = { box: d, inputs: {} };
    for (const [group, list] of AG_SPEC) {
      const g = document.createElement('fieldset'); g.className = 'tn-group';
      const lg = document.createElement('legend'); lg.textContent = group; g.appendChild(lg);
      for (const [k, label, min, max, step] of list) {
        const row = document.createElement('label'); row.className = 'tn-row';
        const name = document.createElement('span'); name.textContent = label;
        const val = document.createElement('output');
        const inp = document.createElement('input'); Object.assign(inp, { type: 'range', min, max, step, value: YTPMAge[k] });
        val.textContent = agFmt(k, YTPMAge[k]);
        inp.addEventListener('input', () => { YTPMAge[k] = parseFloat(inp.value); val.textContent = agFmt(k, YTPMAge[k]); agSync(inp); agDirty(); agApply(); });
        row.append(name, val, inp); g.appendChild(row); set.inputs[k] = { inp, val };
      }
      $('.tn-sliders', d).appendChild(g);
    }
    AG.sets.push(set);
    $('.ag-reset', d).addEventListener('click', () => { Object.assign(YTPMAge, YTPMAgeDefaults); agSync(); agDirty(); agApply(); });
    $('.ag-save', d).addEventListener('click', agSave);
    agLoad();
  }

  /* ------------------------------------------------------------------ v4: bubble tuner (drives YTPMLook: flat badges + 3D bubble) */
  const TN_SPEC = [
    ['Edge', [['edgeWidth', 'Edge width', 0.05, 1.5, 0.01], ['edgeVary', 'Edge unevenness', 0, 1, 0.01], ['edgeColour', 'Edge colour strength', 0, 1.5, 0.01], ['rimLine', 'Thin outline', 0, 1, 0.01]]],
    ['Film', [['saturation', 'Colour saturation', 0, 1.2, 0.01], ['filmInside', 'Film colour in the middle', 0, 1, 0.01], ['filmThick', 'Film thickness (nm)', 80, 700, 1], ['flowSpeed', 'Swirl speed', 0, 2, 0.01], ['reflection', 'Reflections', 0, 1, 0.01]]],
    ['Dots at the edge', [['dotRefr', 'Dot copies left and right', 0, 1, 0.01], ['dotRing', 'Dot ring all round', 0, 1, 0.01], ['refrPeriod', 'Sway time (seconds)', 20, 240, 1]]],
    ['Centre', [['glow', 'Centre light', 0, 1.6, 0.01], ['breath', 'Breath time (seconds)', 4, 40, 0.5]]],
    ['Pulse motion', [['motionAmt', 'Amount', 0, 1, 0.01], ['motionTime', 'Cycle time (seconds)', 4, 40, 0.5]]],
    ['Liquid pulse', [['lqRefr', 'Refraction', 0, 1, 0.01], ['lqRefl', 'Reflections', 0, 1, 0.01], ['lqFilm', 'Thin film colour', 0, 1, 0.01], ['lqTint', 'Logo colour in the water', 0, 1, 0.01], ['lqWobble', 'Surface ripple', 0, 1, 0.01]]],
    ['Tiny bubbles and life', [['lqDrops', 'Tiny bubbles (how many)', 0, 1, 0.01], ['lqDropSize', 'Tiny bubble size', 0.4, 2, 0.01], ['lqMove', 'Logo drift', 0, 1, 0.01], ['turn', 'Bubble sway (degrees)', 0, 40, 1]]]
  ];
  const TN_KEY = 'rm-bubble-look', TN_DOC = 'tuning/bubble';
  try { const ls = JSON.parse(store.get(TN_KEY) || 'null'); if (ls) for (const k in YTPMLookDefaults) if (typeof ls[k] === 'number') YTPMLook[k] = ls[k]; } catch (e) { /* ignore */ }
  const TN = { el: null, c3: null, on: false, raf: 0, yaw: 0, pitch: 8, drag: null, dirty: false, db: null, sets: [] };
  function tunerHtml() {
    return `<section class="card" id="bubble-tuner" aria-labelledby="h-tn"><div class="sec-h"><div class="eyebrow">Emulsion · tune it live</div><h2 id="h-tn">Tune the bubble yourself</h2>
      <p>Move a slider and every Emulsion bubble on this page changes live: the flat badges and the 3D sphere in the full view. Drag the sphere to turn it. Press Save when you like it: I read the saved numbers and put them in the Windows app.</p></div>
      <div class="tuner"><div class="tn-prev"><canvas class="c3" aria-label="Emulsion bubble in 3D, drag to turn"></canvas><div class="sm">${cv({ tier: 7, size: 64 })}${cv({ tier: 7, size: 44 })}</div><div class="cap">3D sphere · badge 64 · list 44</div></div>
      <div class="tn-sliders"></div>
      ${tnActionsHtml(true)}</div></section>`;
  }
  const tnStep = (k) => TN_SPEC.flatMap((g) => g[1]).find((x) => x[0] === k)[4];
  const tnFmt = (k, v) => (tnStep(k) >= 1 ? String(Math.round(v)) : (+v).toFixed(tnStep(k) >= .5 ? 1 : 2));
  function tnRedraw() { try { YTPMBubble.redraw(); } catch (e) { /* ignore */ } try { YTPM3D.redraw(); } catch (e) { /* ignore */ } }
  function tnSync(skip) {
    TN.sets = TN.sets.filter((set) => set.box.isConnected);
    for (const set of TN.sets) for (const k in set.inputs) { if (set.inputs[k].inp === skip) continue; set.inputs[k].inp.value = YTPMLook[k]; set.inputs[k].val.textContent = tnFmt(k, YTPMLook[k]); }
    tnRedraw();
  }
  function tnStatus(t) { TN.statusText = t; $$('.tn-status').forEach((x) => { x.textContent = t; }); }
  /** The slider groups, built into `box`. Every copy (the v4 card and the full view's side panel) drives the same YTPMLook. */
  function tnBuild(box) {
    const set = { box, inputs: {} };
    for (const [group, list] of TN_SPEC) {
      const g = document.createElement('fieldset'); g.className = 'tn-group';
      const lg = document.createElement('legend'); lg.textContent = group; g.appendChild(lg);
      for (const [k, label, min, max, step] of list) {
        const row = document.createElement('label'); row.className = 'tn-row';
        const name = document.createElement('span'); name.textContent = label;
        const val = document.createElement('output');
        const inp = document.createElement('input'); Object.assign(inp, { type: 'range', min, max, step, value: YTPMLook[k] });
        val.textContent = tnFmt(k, YTPMLook[k]);
        inp.addEventListener('input', () => { YTPMLook[k] = parseFloat(inp.value); val.textContent = tnFmt(k, YTPMLook[k]); tnSync(inp); tnDirty(); });
        row.append(name, val, inp); g.appendChild(row); set.inputs[k] = { inp, val };
      }
      box.appendChild(g);
    }
    TN.sets.push(set);
  }
  function tnReset() { Object.assign(YTPMLook, YTPMLookDefaults); tnSync(); tnDirty(); }
  async function tnSave() {
    if (!TN.db) return;
    $$('.tn-save').forEach((b) => { b.disabled = true; }); tnStatus('Saving…');
    try {
      const out = {}; for (const k in YTPMLookDefaults) out[k] = +YTPMLook[k];
      out.savedAt = new Date().toISOString();
      await TN.db.doc(TN_DOC).set(out);
      TN.dirty = false; tnStatus('Saved. Claude will use these settings.');
    } catch (e) { tnStatus('Could not save (' + ((e && e.code) || 'error') + '). Changes stay in this browser.'); }
    $$('.tn-save').forEach((b) => { b.disabled = !TN.db; });
  }
  function tnActionsHtml(open) { return `<div class="tn-actions"><button type="button" class="tn-save"${TN.db ? '' : ' disabled'}>Save for Claude</button><button type="button" class="tn-reset">Reset</button>${open ? '<button type="button" class="tn-open">Open full view</button>' : ''}<span class="tn-status" role="status">${esc(TN.statusText || 'Not changed yet')}</span></div>`; }
  function tnBindActions(root) { $('.tn-reset', root).addEventListener('click', tnReset); $('.tn-save', root).addEventListener('click', tnSave); }
  /** Side panel in the full view (Emulsion): the same sliders, so the big bubble can be tuned while you look at it. */
  function tnSidePanel(cardEl) {
    const d = document.createElement('div'); d.className = 'fv-tune';
    d.innerHTML = `<div class="mt" style="color:#b49cff">Tune the bubble</div><div class="tn-sliders"></div>${tnActionsHtml(false)}`;
    cardEl.appendChild(d);
    tnBuild($('.tn-sliders', d)); tnBindActions(d);
  }
  function tnDirty() { TN.dirty = true; tnStatus(TN.db ? 'Changed, not saved yet' : 'Changed. Kept in this browser.'); store.set(TN_KEY, JSON.stringify(YTPMLook)); }
  function tnTick(now) {
    TN.raf = 0;
    if (!TN.on) return;
    const sw = reduced() ? 0 : +YTPMLook.turn || 0, ts = now / 1000;
    try { YTPM3D.rot(TN.yaw + sw * Math.sin(ts * .45), TN.pitch + sw * .35 * Math.sin(ts * .31), 0); } catch (e) { /* ignore */ }
    TN.raf = requestAnimationFrame(tnTick);
  }
  function tnStart() {
    if (!TN.el || !TN.el.isConnected || TN.el.closest('[hidden]') || !fv.hidden) return;
    tnStop();
    const old = $('.c3', TN.el), c = old.cloneNode(false);   // fresh canvas: stop() loses the old context
    old.replaceWith(c); TN.c3 = c; tnBindDrag(c);
    const px = Math.round(Math.min(c.clientWidth || 210, 420) * Math.min(window.devicePixelRatio || 1, 2));
    c.width = c.height = px;
    TN.on = YTPM3D.start(c, 'bubble', reduced());
    if (TN.on) { YTPM3D.rot(TN.yaw, TN.pitch, 0); TN.raf = requestAnimationFrame(tnTick); }
    else c.replaceWith(Object.assign(document.createElement('div'), { className: 'c3 is-nogl' }));
  }
  function tnStop() { if (!TN.on) return; TN.on = false; cancelAnimationFrame(TN.raf); TN.raf = 0; try { if (YTPM3D.canvas() === TN.c3) YTPM3D.stop(); } catch (e) { /* ignore */ } }
  function tnBindDrag(c) {
    c.addEventListener('pointerdown', (ev) => { TN.drag = { x: ev.clientX, y: ev.clientY }; try { c.setPointerCapture(ev.pointerId); } catch (e) { /* ignore */ } });
    c.addEventListener('pointermove', (ev) => { const g = TN.drag; if (!g) return; TN.yaw += (ev.clientX - g.x) * .7; TN.pitch = clamp(TN.pitch + (ev.clientY - g.y) * .7, -85, 85); g.x = ev.clientX; g.y = ev.clientY; });
    const end = () => { TN.drag = null; };
    c.addEventListener('pointerup', end); c.addEventListener('pointercancel', end);
  }
  const tnVbar = () => { const w = $('.vbar-wrap'); if (w) document.documentElement.style.setProperty('--vbar-h', w.offsetHeight + 'px'); };
  function bindTuner(p) {
    tnVbar();
    const el = $('#bubble-tuner', p); if (!el) return;
    TN.el = el;
    tnBuild($('.tn-sliders', el));
    tnBindActions(el);
    const openB = $('.tn-open', el);
    openB.addEventListener('click', () => openFV(showcaseList(), 6, openB));
    const save = $('.tn-save', el);
    save.disabled = true; tnStatus('Connecting…');
    (async () => {
      let db = null;
      try { db = window.claude && window.claude.use ? await window.claude.use('db') : null; } catch (e) { db = null; }
      if (!db) { tnStatus(TN.dirty ? 'Changed. Kept in this browser (saving for Claude is not available here).' : 'Saving for Claude is not available here. Changes stay in this browser.'); return; }
      TN.db = db; $$('.tn-save').forEach((b) => { b.disabled = false; }); agLoad();
      try {
        const snap = await db.doc(TN_DOC).get();
        if (snap.exists && !TN.dirty) { const v = snap.data() || {}; for (const k in YTPMLookDefaults) if (typeof v[k] === 'number') YTPMLook[k] = v[k]; tnSync(); tnStatus('Showing your saved settings'); return; }
      } catch (e) { /* keep current */ }
      tnStatus(TN.dirty ? 'Changed, not saved yet' : 'Not changed yet');
    })();
    requestAnimationFrame(tnStart);
  }
  function buildV4() { return verifyHtml() + tunerHtml() + buildV2(BUILD_STATUS, '-4'); }

  /* ------------------------------------------------------------------ v1: the three theme options (settings only) */
  const OPTIONS = [
    { id: 'badge', letter: 'A', name: 'Case Badges', world: 'PC builders show off their parts with the small metal badges on the front of the case: the CPU sticker, the GPU sticker, “Designed for Windows”. Here every award is one of those badges, and a full set of four becomes a <b>Certified Build</b> plate on the case panel.', shape: 'plaque', icon: 'cpu', hero: 'gold', plate: 'Certified Gold · The Watchtower', sets: 'The Watchtower (CPU, memory, GPU, disk active) · The Pipeline (upload, download, data tracked, adapters) · The Caretaker (alerts handled, tasks ended, cleanups, reports)' },
    { id: 'medal', letter: 'B', name: 'Uptime Medals', world: 'Data centres and sysadmins celebrate uptime: 99.9%, then 99.99%, then five nines. Every award is a struck medal with a spun face and the icon engraved, and a set of four fills a <b>1U rack plate</b> like the ones that hold the real servers.', shape: 'round', icon: 'clock', hero: 'platinum', plate: 'Gold rack · Rack 01: Watch', sets: 'Rack 01: Watch (CPU, memory, GPU, disk) · Rack 02: Flow (network, data, adapters, cap) · Rack 03: Care (alerts, tasks, cleanups, reports)', picked: 1 },
    { id: 'hof', letter: 'C', name: 'Hall of Fame Plaques', world: 'Benchmark and overclocking leaderboards put the best machines in a <b>Hall of Fame</b>. Every award is a standing plaque with the details etched under a mirror pocket, and a set of four becomes a leaderboard frame on your wall.', shape: 'award', icon: 'activity', hero: 'gold', plate: 'Hall of Fame Gold · Sentinel', award: true, sets: 'Leaderboard: Sentinel (CPU, memory, GPU, disk) · Leaderboard: Bandwidth (network, data, adapters, cap) · Leaderboard: Steward (alerts, tasks, cleanups, reports)' }
  ];
  function buildV1() {
    const sec = OPTIONS.map((o) => {
      const heroC = o.award ? cv({ kind: o.hero, shape: o.shape, icon: o.icon, size: 220, pre: 'Presented for', title: 'Night Watch', line: '1,000 hours of watching', sub: 'Gold · 21 Nov 2026' }) : cv({ kind: o.hero, shape: o.shape, icon: o.icon, size: 220 });
      const tiers = TIER.map((n, i) => {
        const c = i === 6 ? cv({ tier: 7, size: 84 }) : cv(Object.assign({ tier: i + 1, shape: o.shape, icon: o.icon, size: 84 }, o.award ? { pre: 'Presented for', title: 'Night Watch', line: n + ' tier', sub: 'Resource Monitor' } : {}));
        return `<figure class="otier"><div class="slot">${c}</div><figcaption>${n}</figcaption></figure>`;
      }).join('');
      return `<section class="opt" id="opt-${o.id}" aria-labelledby="oh-${o.id}"><div class="opt-head"><span class="letter" aria-hidden="true">${o.letter}</span><div><h2 id="oh-${o.id}">${o.name}${o.picked ? ' <span class="eyebrow" style="margin-left:8px">picked in v2</span>' : ''}</h2><p class="world">${o.world}</p></div></div>
        <div class="opt-body"><div class="ohero">${heroC}<p class="plate">${o.plate}</p></div><div class="ladder" role="list" aria-label="Seven tiers">${tiers}</div></div>
        <p class="sets"><span>Sets</span>${o.sets}</p></section>`;
    }).join('');
    return `<header><div class="eyebrow">Earlier options</div><h1 style="font-size:clamp(26px,4vw,40px);margin:6px 0 8px">Pick the award for Resource Monitor</h1><p style="color:var(--dim);max-width:68ch">Every badge, tier and rule stays the same. Only the award itself changes, and it should come from your users’ world: people who build, tune and watch their PCs. Each option is rendered live with the real metal, gem and Emulsion bubble shaders (the bubble carries the Resource Monitor pulse).</p>
      <div class="chips"><span>Free mode · no paid plan</span><span>Emulsion earned over ~a year</span><span>7 tiers · 3 sets of 4</span></div></header>${sec}
      <p class="note">Previews use the reference shapes from the achievements plugin. Diamond and Ruby stay real cut stones in every option, and Emulsion is always the same soap bubble.</p>`;
  }

  function bindPanel(p) {
    $$('[data-tile]', p).forEach((b) => b.addEventListener('click', () => openFV(showcaseList(), +b.dataset.tile, b)));
    p.addEventListener('click', (ev) => {
      const row = ev.target.closest('.row.earned');
      if (row) { openFV(ORDER.map((id) => ENT[id]), ORDER.indexOf(row.dataset.e), row); return; }
      const more = ev.target.closest('[data-more]');
      if (more) expand(more);
    });
    p.addEventListener('keydown', (ev) => {
      const row = ev.target.closest && ev.target.closest('.row.earned');
      if (row && ev.target === row && (ev.key === 'Enter' || ev.key === ' ') && !ev.repeat) { ev.preventDefault(); openFV(ORDER.map((id) => ENT[id]), ORDER.indexOf(row.dataset.e), row); }
    });
    const m = $('#btnMoment', p); if (m) m.addEventListener('click', () => openMoment(m));
    const l = $('#btnLevel', p); if (l) l.addEventListener('click', () => openLevel(l));
    const v1 = $('#btnV1', p); if (v1) v1.addEventListener('click', () => RMC.show('v1', true));
    bindTuner(p);
  }
  function expand(btn) {
    const C = CATS.find((c) => c.id === btn.dataset.more), card = btn.closest('.cat'), rows = $('.rows', card);
    const open = btn.getAttribute('aria-expanded') === 'true';
    if (!open) { rows.insertAdjacentHTML('beforeend', C.ids.slice(C.first).map(rowHtml).join('')); mountIn(rows); btn.setAttribute('aria-expanded', 'true'); btn.textContent = 'Show fewer'; }
    else { $$('.row', rows).slice(C.first).forEach((r) => r.remove()); btn.setAttribute('aria-expanded', 'false'); btn.textContent = 'Show all ' + C.ids.length; }
  }
  /* ------------------------------------------------------------------ full view */
  const fv = $('#fv'), stage = $('#fvStage'), slab = $('#fvSlab'), front = $('#fvFront'), back = $('#fvBack'), edge = $('#fvEdge'), floor = $('#fvFloor'), card = $('#fvCard');
  const FV = { list: [], i: 0, opener: null, flip: 0, flipT: 0, rx: 0, ry: 0, px: null, py: 0, moved: 0, raf: 0, last: 0, canvas: null, bubble: false, edgeT: 0, openedAt: 0,
    is3d: false, yaw: 0, pitch: -18, vy: 0, vp: 0, drag: null, dragged: 0, turn: 0, turnT: 0, lastDrag: 0 };
  // Diamond, Ruby and Emulsion turn as real 3D objects in the full view (ray traced); the metal medals keep the struck slab with its spec-sheet back
  const MET3 = { bronze: [0.86, 0.52, 0.30], silver: [0.93, 0.94, 0.97], gold: [1.0, 0.77, 0.33], platinum: [0.72, 0.93, 0.91] };   // = YTPMGem METAL
  const showcaseList = () => TIER.map((n, i) => ({ id: 'tier' + i, kind: 'showcase', title: n + ' medal', line: i === 6 ? 'The live soap bubble, the rarest thing in the tool.' : `What a ${n} medal looks like. Every ladder badge climbs to it.`, tier: i + 1, icon: 'clock', earned: true }));
  function stageSize(bub) {
    const w = window.innerWidth, h = window.innerHeight, narrow = w <= 760;
    let s = narrow ? Math.min(w * .88, h * .46) : Math.min(w * .44, h * .76);
    s = Math.min(s, 600);
    return Math.max(210, Math.round(s));
  }
  function backHtml(e, bub) {
    const earned = e.kind === 'showcase' ? 'Preview' : e.kind === 'single' ? demoDate(e.id, 0) : demoDate(e.id, e.tier - 1);
    return `<div class="cardback${bub ? ' b' : ''}" data-tier="${e.tier}"><div class="brand"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 12h4.6l2-3.7 1.8 7.3 1.8-8.6 1.7 5H21"/></svg>Resource Monitor</div>
      <div class="kick">Certificate of achievement</div><div class="ttl">${esc(e.title)}</div><div class="for">${esc(e.kind === 'ladder' ? e.line : e.line)}</div>
      <dl><div><dt>Tier</dt><dd>${TIER[e.tier - 1]}</dd></div><div><dt>Earned</dt><dd>${esc(earned)}</dd></div><div><dt>No.</dt><dd>${certNo(e.id, e.tier)}</dd></div></dl>
      <div class="nt">Earned by using Resource Monitor, never bought. Only confirmed changes count.</div><div class="ft">Resource Monitor by Rajhans</div></div>`;
  }
  function cardHtml(e) {
    let notes = '';
    if (e.kind === 'ladder') {
      for (let i = 0; i < e.tier; i++) notes += `<li><b>${TIER[i]}</b> · ${demoDate(e.id, i)}</li>`;
      notes += `<li>Your count: <b>${fmt(e.L.n)}</b> ${esc(e.L.what)}</li>`;
      notes += `<li>${e.tier < 6 ? `Next: <b>${TIER[e.tier]}</b> at ${fmt(e.s.next)}` : 'Emulsion: gate shut'}</li>`;
    } else if (e.kind === 'single') notes = `<li>Earned <b>${demoDate(e.id, 0)}</b></li><li>+${e.B.xp} XP</li>`;
    else notes = '<li>A preview of the tier, not an earned badge.</li>';
    return `<div class="fv-count">${FV.i + 1} of ${FV.list.length}</div><div class="mt" style="color:${THEX[e.tier - 1]}">Uptime · ${TIER[e.tier - 1]}</div><h2>${esc(e.title)}</h2><p class="ln">${esc(e.line)}</p>${pipsHtml(e.tier)}<ul>${notes}</ul>${e.tier < 7 ? wearHtml(e) : ''}<p class="hint">${FV.is3d ? 'Drag to turn it any way, like holding it. Click, Enter or Space turns it over. ← → go through the list, Esc closes.' : 'Tilt with the pointer. Click, Enter or Space turns the medal to its spec sheet. ← → go through the list, Esc closes.'}</p>`;
  }
  function wearWords(w) {
    if (w.clean) return 'Spotless. It shines like the day you earned it.';
    const dust = w.d < .08 ? '' : w.d < .3 ? 'a little dust' : w.d < .6 ? 'dusty' : 'thick with dust';
    const scr = w.s < .04 ? '' : w.s < .15 ? 'a few hairline scratches' : w.s < .35 ? 'scratched' : 'well worn';
    const web = w.w < .02 ? '' : w.w < .35 ? 'a cobweb starting' : w.w < .75 ? 'cobwebs' : 'thick with cobwebs';
    const days = Math.floor(w.days), when = w.n ? `Polished ${days === 0 ? 'today' : days === 1 ? '1 day ago' : days + ' days ago'}` : `On the shelf ${days} days`;
    return `${when} · ${[dust, web].filter(Boolean).join(', ') || 'nearly clean'}`;
  }
  const wearColour = (c) => c >= 90 ? '#6fd38a' : c >= 70 ? '#c9d46a' : c >= 50 ? '#e0a84f' : '#d9785a';
  function wearHtml(e) {
    const w = wearOf(e.id);
    return `<div class="wr" id="wr"><div class="wr-h"><span>Condition</span><b id="wrPct">${w.cond}%</b></div>
      <div class="wr-bar" role="progressbar" aria-label="Condition" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${w.cond}" id="wrBarR"><i id="wrBar" style="width:${w.cond}%;--wc:${wearColour(w.cond)}"></i></div>
      <div class="wr-sub" id="wrSub" aria-live="polite">${esc(wearWords(w))}</div>
      <div class="wr-row"><button type="button" class="wr-btn${w.clean ? ' done' : ''}" id="wrPolish">${w.clean ? 'Spotless ✦' : 'Polish ✦'}</button><button type="button" class="wr-ff" id="wrAge" title="Test: one more day on the shelf">+1 day</button><button type="button" class="wr-ff" id="wrAge10" title="Test: ten more days on the shelf">+10 days</button></div>
      <div class="wr-note">Ages like the real thing: 1 hour here is 1 day on the shelf. Cobwebs start after 20 days. One rub clears the web (it grows back), each rub takes 30% of the dust. Shine <b id="wrShine">${WEAR._.shine || 0}</b> ✦</div></div>`;
  }
  function wearUI(w) {
    const set = (id, f) => { const el = document.getElementById(id); if (el) f(el); };
    set('wrPct', (el) => { el.textContent = w.cond + '%'; });
    set('wrBar', (el) => { el.style.width = w.cond + '%'; el.style.setProperty('--wc', wearColour(w.cond)); });
    set('wrBarR', (el) => el.setAttribute('aria-valuenow', w.cond));
    set('wrSub', (el) => { el.textContent = wearWords(w); });
    set('wrPolish', (el) => { el.textContent = w.clean ? 'Spotless ✦' : 'Polish ✦'; el.classList.toggle('done', w.clean); });
    set('wrShine', (el) => { el.textContent = WEAR._.shine || 0; });
  }
  function wearRefresh(e, w) {
    try { YTPM3D.wear([w.d, w.sd, w.w]); } catch (err) { /* ignore */ }
    if (FV.canvas && FV.canvas.dataset.wid) { try { YTPMGem.set(FV.canvas, { wear: [w.d, w.sd, w.w] }); } catch (err) { /* ignore */ } }
    applyWear(document);
    $$(`.row[data-e="${e.id}"] .rtitle`).forEach((t) => { const old = $('.dusty', t); if (old) old.remove(); t.insertAdjacentHTML('beforeend', dustyTag(e.id)); });
    wearUI(w);
  }
  function shine(gain) {
    if (!reduced()) {
      const d = document.createElement('div'); d.className = 'fv-shine';
      let h = '<i class="sw"></i>';
      for (let k = 0; k < 5; k++) { const a = Math.random() * 6.283, r = 18 + Math.random() * 28; h += `<b style="left:${(50 + Math.cos(a) * r).toFixed(1)}%;top:${(50 + Math.sin(a) * r).toFixed(1)}%;animation-delay:${(k * .08).toFixed(2)}s"></b>`; }
      d.innerHTML = h; stage.appendChild(d); setTimeout(() => d.remove(), 1000);
      const wr = $('#wr');
      if (wr && gain) { const pop = document.createElement('span'); pop.className = 'wr-pop'; pop.textContent = gain; wr.appendChild(pop); setTimeout(() => pop.remove(), 950); }
    }
  }
  function doPolish() {
    const e = FV.list[FV.i]; if (!e || e.tier === 7) return;
    const was = wearOf(e.id);
    if (was.clean) { shine(''); return; }   // already spotless: a sparkle, no points
    const w = polish(e.id);
    let gain = 1;
    if (w.clean) gain += 10;               // finishing the job: bonus
    WEAR._.shine = (WEAR._.shine || 0) + gain; saveWear();
    shine(w.clean ? `Spotless! +${gain} ✦` : `+${gain} ✦`);
    wearRefresh(e, w);
  }
  card.addEventListener('click', (ev) => {
    const t = ev.target.closest && ev.target.closest('button'); if (!t) return;
    if (t.id === 'wrPolish') doPolish();
    else if (t.id === 'wrAge' || t.id === 'wrAge10') { const e = FV.list[FV.i]; if (e && e.tier < 7) { let w; for (let k = t.id === 'wrAge' ? 1 : 10; k > 0; k--) w = ageDay(e.id); wearRefresh(e, w); } }
  });
  function renderFV() {
    const e = FV.list[FV.i], bub = e.tier === 7, S = stageSize(bub);
    FV.bubble = bub;
    stage.style.setProperty('--S', S + 'px'); stage.style.setProperty('--T', (S * .034).toFixed(1) + 'px');
    try { YTPM3D.stop(); } catch (err) { /* ignore */ }
    FV.is3d = false;
    if (window.YTPM3D) {
      const c = document.createElement('canvas'), px = Math.round(Math.min(S * Math.min(window.devicePixelRatio || 1, 1.5), 900));
      c.width = c.height = px; c.className = 'ach-3d'; c.setAttribute('aria-hidden', 'true');
      front.innerHTML = ''; front.appendChild(c);
      const w = bub ? { d: 0, s: 0, w: 0 } : wearOf(e.id);
      const sub = TIER[e.tier - 1] + ' · ' + (e.kind === 'showcase' ? 'Uptime' : e.kind === 'single' ? demoDate(e.id, 0) : demoDate(e.id, e.tier - 1));
      FV.is3d = YTPM3D.start(c, bub ? 'bubble' : e.tier === 6 ? 'ruby' : e.tier === 5 ? 'dia' : 'medal', reduced(),
        { metal: MET3[KIND[e.tier - 1]], icon: e.icon, title: e.title, sub, wear: [w.d, w.sd, w.w] });
    }
    stage.classList.toggle('is3d', FV.is3d);
    if (FV.is3d) {
      back.innerHTML = ''; edge.innerHTML = '';
      FV.yaw = 0; FV.pitch = bub ? 0 : e.tier < 5 ? -12 : -24; FV.vy = FV.vp = 0; FV.turn = FV.turnT = 0; FV.lastDrag = 0;
      slab.setAttribute('aria-label', `${e.title}, ${TIER[e.tier - 1]}. Drag to turn it in 3D; Enter or Space turns it over`);
      card.innerHTML = cardHtml(e);
      card.classList.add('has-tune');
      if (bub) tnSidePanel(card); else agSidePanel(card, e);
      $('#fvP').hidden = $('#fvN').hidden = FV.list.length < 2;
      FV.canvas = null;
      apply();
      return;
    }
    front.innerHTML = cv({ tier: e.tier, icon: e.icon, size: S, dpr: 2, wid: e.id }) + `<div class="fv-glare${bub ? ' b' : ''}"></div>`;
    back.innerHTML = backHtml(e, bub);
    edge.innerHTML = '';
    FV.flip = FV.flipT = 0;
    slab.setAttribute('aria-label', `${e.title}, ${TIER[e.tier - 1]}. Click to turn the medal over to its spec sheet`);
    card.innerHTML = cardHtml(e);
    card.classList.add('has-tune');
    if (bub) tnSidePanel(card); else agSidePanel(card, e);
    $('#fvP').hidden = $('#fvN').hidden = FV.list.length < 2;
    FV.canvas = $('canvas', front);
    mountIn(front);
    try { YTPMGem.only(FV.canvas); YTPMBubble.only(FV.canvas); } catch (err) { /* ignore */ }
    clearTimeout(FV.edgeT);
    FV.edgeT = setTimeout(() => makeEdge(FV.canvas), 350);
    apply();
  }
  function makeEdge(c) {   // 12 darkened copies of the outline along Z (made once from the first frame, no live CSS filters)
    try {
      const N = 256, a = document.createElement('canvas'); a.width = a.height = N;
      const g = a.getContext('2d'); g.drawImage(c, 0, 0, N, N);
      const base = g.getImageData(0, 0, N, N), urls = [.5, .36, .28].map((f) => {
        const out = g.createImageData(N, N), s = base.data, d = out.data;
        for (let i = 0; i < s.length; i += 4) { d[i] = s[i] * f; d[i + 1] = s[i + 1] * f; d[i + 2] = s[i + 2] * f; d[i + 3] = s[i + 3]; }
        const b = document.createElement('canvas'); b.width = b.height = N; b.getContext('2d').putImageData(out, 0, 0); return b.toDataURL('image/png');
      });
      const T = parseFloat(getComputedStyle(stage).getPropertyValue('--T')) || 10;
      let h = '';
      for (let k = 0; k < 12; k++) h += `<i style="background-image:url(${urls[Math.min(2, Math.floor(k / 4))]});transform:translateZ(${(T / 2 - (k + 1) * T / 13).toFixed(2)}px)"></i>`;
      edge.innerHTML = h;
    } catch (err) { edge.innerHTML = ''; }
  }
  function apply() {
    if (FV.is3d) {
      slab.style.transform = 'none';
      floor.style.transform = 'none';
      try { YTPM3D.rot(FV.yaw + FV.ry * .6, FV.pitch + FV.turn - FV.rx * .6, 0); } catch (err) { /* ignore */ }
      return;
    }
    slab.style.transform = `rotateX(${FV.rx.toFixed(2)}deg) rotateY(${(FV.flip + FV.ry).toFixed(2)}deg)`;
    const gl = $('.fv-glare', front);
    if (gl) { gl.style.setProperty('--gx', (50 + FV.ry / 22 * 40).toFixed(1) + '%'); gl.style.setProperty('--gy', (35 - FV.rx / 16 * 30).toFixed(1) + '%'); }
    const rad = (FV.flip + FV.ry) * Math.PI / 180;
    floor.style.transform = `translateX(${(FV.ry * 1.2).toFixed(1)}px) scaleX(${(1 - .28 * Math.abs(Math.sin(rad))).toFixed(3)})`;
    if (FV.canvas && !FV.bubble && FV.canvas.dataset.live === '1') { try { YTPMGem.set(FV.canvas, { tilt: [FV.ry / 22 * .5, FV.rx / 16 * .5] }); } catch (err) { /* ignore */ } }
  }
  function tick(ts) {
    FV.raf = requestAnimationFrame(tick);
    const dt = Math.min(.05, (ts - (FV.last || ts)) / 1000); FV.last = ts;
    let tx, ty;
    if (FV.px !== null && ts - FV.moved < 4000) {
      const r = stage.getBoundingClientRect();
      const hv = FV.bubble ? 1 : YTPMAge.hover;   // metals and stones: a gentle hover tilt (tuner)
      ty = clamp((FV.px - (r.left + r.width / 2)) / (window.innerWidth / 2), -1, 1) * 22 * hv;
      tx = -clamp((FV.py - (r.top + r.height / 2)) / (window.innerHeight / 2), -1, 1) * 16 * hv;
    } else { const t = ts / 1000, sw = FV.bubble ? (+YTPMLook.turn || 0) / 9 : YTPMAge.sway; tx = 3.5 * sw * Math.sin(.7 * t); ty = 9 * sw * Math.sin(.5 * t); }
    const k = 1 - Math.exp(-dt * 16);
    FV.rx += (tx - FV.rx) * k; FV.ry += (ty - FV.ry) * k;
    FV.flip += (FV.flipT - FV.flip) * (1 - Math.exp(-dt * 6.5));
    if (Math.abs(FV.flipT - FV.flip) < .05) FV.flip = FV.flipT;
    if (FV.is3d) {
      if (!FV.drag) {
        FV.yaw += FV.vy * dt; FV.pitch += FV.vp * dt;                  // a flick keeps turning, then slows
        const fr = Math.exp(-dt * (FV.bubble ? 2.2 : YTPMAge.friction)); FV.vy *= fr; FV.vp *= fr;
      }
      FV.turn += (FV.turnT - FV.turn) * (1 - Math.exp(-dt * 5));
      if (FV.drag) { FV.rx *= .9; FV.ry *= .9; }
    }
    apply();
  }
  function flipIt() {
    if (FV.is3d) { if (Date.now() - FV.dragged < 250) return; FV.turnT += 180; if (reduced()) { FV.turn = FV.turnT; apply(); } return; }
    FV.flipT = FV.flipT === 0 ? 180 : 0;
    if (reduced()) { FV.flip = FV.flipT; apply(); }
  }
  function openFV(list, i, opener) {
    closeOverlays(); tnStop();
    FV.list = list; FV.i = clamp(i, 0, list.length - 1); FV.opener = opener || document.activeElement; FV.openedAt = Date.now();
    fv.hidden = false; document.documentElement.setAttribute('data-ach-hold', '');
    document.body.style.overflow = 'hidden';
    FV.rx = FV.ry = 0; FV.px = null;
    renderFV();
    if (!reduced()) { FV.last = 0; FV.raf = requestAnimationFrame(tick); }
    slab.focus();
  }
  function closeFV() {
    if (fv.hidden) return;
    fv.hidden = true; cancelAnimationFrame(FV.raf); FV.raf = 0; clearTimeout(FV.edgeT);
    document.documentElement.removeAttribute('data-ach-hold'); document.body.style.overflow = '';
    try { YTPMGem.only(null); YTPMBubble.only(null); YTPM3D.stop(); } catch (err) { /* ignore */ }
    FV.is3d = false; stage.classList.remove('is3d');
    front.innerHTML = ''; back.innerHTML = ''; edge.innerHTML = ''; FV.canvas = null;
    if (FV.opener && FV.opener.isConnected) FV.opener.focus();
    if (RMC.cur() === 'v4') requestAnimationFrame(tnStart);
  }
  function stepFV(d) { if (FV.list.length < 2) return; FV.i = (FV.i + d + FV.list.length) % FV.list.length; renderFV(); }
  fv.addEventListener('pointermove', (ev) => { FV.px = ev.clientX; FV.py = ev.clientY; FV.moved = performance.now(); });
  fv.addEventListener('pointerleave', () => { FV.px = null; });
  fv.addEventListener('click', (ev) => { if (ev.target === fv || ev.target.id === 'fvMain') closeFV(); });
  slab.addEventListener('click', flipIt);
  slab.addEventListener('pointerdown', (ev) => {
    if (!FV.is3d) return;
    FV.drag = { x: ev.clientX, y: ev.clientY, t: performance.now(), sx: ev.clientX, sy: ev.clientY };
    FV.vy = FV.vp = 0;
    try { slab.setPointerCapture(ev.pointerId); } catch (err) { /* ignore */ }
  });
  slab.addEventListener('pointermove', (ev) => {
    const g = FV.drag; if (!FV.is3d || !g) return;
    const now = performance.now(), dx = ev.clientX - g.x, dy = ev.clientY - g.y, k = 300 / Math.max(200, stage.clientWidth);
    FV.yaw += dx * k; FV.pitch += dy * k;   // vertical drag inverted (the owner 11-22)
    const dt = Math.max(8, now - g.t) / 1000;
    FV.vy = FV.vy * .5 + (dx * k / dt) * .5; FV.vp = FV.vp * .5 + (dy * k / dt) * .5;
    g.x = ev.clientX; g.y = ev.clientY; g.t = now; FV.lastDrag = now;
    if (Math.hypot(ev.clientX - g.sx, ev.clientY - g.sy) > 5) FV.dragged = Date.now();
    if (reduced()) apply();
  });
  const endDrag = () => { if (!FV.drag) return; if (performance.now() - FV.drag.t > 80) FV.vy = FV.vp = 0; else if (!FV.bubble) { FV.vy *= YTPMAge.flick; FV.vp *= YTPMAge.flick; } FV.drag = null; FV.lastDrag = performance.now(); };
  slab.addEventListener('pointerup', endDrag); slab.addEventListener('pointercancel', endDrag);
  $('#fvX').addEventListener('click', closeFV);
  $('#fvP').addEventListener('click', () => stepFV(-1));
  $('#fvN').addEventListener('click', () => stepFV(1));

  /* ------------------------------------------------------------------ moments */
  const mo = $('#mo'), lu = $('#lu');
  const MOMENTS = [
    { eyebrow: 'Badge upgraded', title: 'Watchkeeper', tier: 4, icon: 'clock', line: '1,000 active hours watched', xp: 250, more: '+1 more badge · Level 12 reached' },
    { eyebrow: 'Unlocked', title: 'First Gigabyte', tier: 1, icon: 'box', line: '1 GB of traffic tracked.', xp: 25, more: '' },
    { eyebrow: 'Badge upgraded', title: 'Data Mover', tier: 6, icon: 'disk', line: '100,000 GB read and written to disk', xp: 1500, more: '+2 more badges · New rank: Ruby' }
  ];
  let momI = 0, moT = 0, moAt = 0, moOpener = null, luT = 0, luAt = 0, luOpener = null;
  function openMoment(opener) {
    closeOverlays();
    const m = MOMENTS[momI++ % MOMENTS.length], S = Math.round(Math.min(300, window.innerWidth * .62));
    moOpener = opener; moAt = Date.now();
    mo.style.setProperty('--hc', THEX[m.tier - 1]); mo.style.setProperty('--MS', S + 'px');
    mo.innerHTML = `<div class="mo-box"><div class="mo-halo"></div><div class="mo-badge">${cv({ tier: m.tier, icon: m.icon, size: S })}</div>
      <div class="mo-t"><div class="eb">${m.eyebrow}</div><h2>${esc(m.title)}</h2><div class="mm" style="color:${THEX[m.tier - 1]}">${TIER[m.tier - 1]}</div><div class="ln">${esc(m.line)}</div><div><span class="xp">+${fmt(m.xp)} XP</span></div><div class="hint">Click, Esc, Enter or Space to close</div><div class="more">${esc(m.more)}</div></div></div><div class="mo-bar" aria-hidden="true"><i></i></div>`;
    mo.hidden = false; mountIn(mo); mo.focus();
    clearTimeout(moT); moT = setTimeout(closeMoment, 6000);
  }
  function closeMoment() { if (mo.hidden) return; clearTimeout(moT); mo.hidden = true; mo.innerHTML = ''; if (moOpener && moOpener.isConnected) moOpener.focus(); }
  function openLevel(opener) {
    closeOverlays();
    luOpener = opener; luAt = Date.now();
    lu.style.setProperty('--hc', THEX[3]);
    let rays = ''; for (let i = 0; i < 18; i++) rays += `<i class="ray" style="--i:${i};--a:${i * 20}deg"></i>`;
    lu.innerHTML = `${rays}<div class="lu-c"><div class="eb">Level up</div><div class="num">16</div><div class="tt">Engineer</div><div class="nr">New rank: Platinum</div></div>`;
    lu.hidden = false; lu.focus();
    clearTimeout(luT); luT = setTimeout(closeLevel, reduced() ? 1600 : 3200);
  }
  function closeLevel() { if (lu.hidden) return; clearTimeout(luT); lu.hidden = true; lu.innerHTML = ''; if (luOpener && luOpener.isConnected) luOpener.focus(); }
  function closeOverlays() { closeMoment(); closeLevel(); }
  mo.addEventListener('click', closeMoment); lu.addEventListener('click', closeLevel);

  document.addEventListener('keydown', (ev) => {
    if (!lu.hidden) { if (Date.now() - luAt > 300 && !ev.repeat) { ev.preventDefault(); closeLevel(); } return; }
    if (!mo.hidden) { if (Date.now() - moAt > 300 && !ev.repeat && ['Escape', 'Enter', ' '].includes(ev.key)) { ev.preventDefault(); closeMoment(); } return; }
    if (fv.hidden) return;
    if (ev.key !== 'Escape' && ev.target && ev.target.closest && ev.target.closest('input, button, .fv-tune')) return;
    if (ev.key === 'Escape') { ev.preventDefault(); closeFV(); }
    else if (ev.key === 'ArrowLeft') { ev.preventDefault(); stepFV(-1); }
    else if (ev.key === 'ArrowRight') { ev.preventDefault(); stepFV(1); }
    else if ((ev.key === 'Enter' || ev.key === ' ') && ev.target === slab && !ev.repeat) { ev.preventDefault(); flipIt(); }
    else if (ev.key === 'Tab') {
      const f = [slab, $('#wrPolish'), $('#wrAge'), $('#wrAge10'), $('#fvP'), $('#fvN'), $('#fvX')].filter((b) => b && !b.hidden), i = f.indexOf(document.activeElement);
      ev.preventDefault(); f[(i + (ev.shiftKey ? -1 : 1) + f.length) % f.length].focus();
    }
  });
  window.addEventListener('resize', () => {
    if (fv.hidden) return;
    const e = FV.list[FV.i], S = stageSize(e.tier === 7);
    stage.style.setProperty('--S', S + 'px'); stage.style.setProperty('--T', (S * .034).toFixed(1) + 'px');
  });

  RMC.ver = {
    build: { v5: buildV4, v4: buildV4, v3: buildV3, v2: () => buildV2(), v1: buildV1 },
    mountIn, bindPanel,
    onShow(id) { if (id === 'v4') { if (TN.el && !TN.on) requestAnimationFrame(tnStart); } else tnStop(); }
  };
})();
