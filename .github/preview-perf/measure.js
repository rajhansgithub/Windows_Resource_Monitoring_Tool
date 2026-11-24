// Load-time measurement for the achievements preview (headless Chrome). node measure.js <file|url> <label> [args...]
// Prints long tasks, per-program shader compile time (time spent blocked in the first status query), and the same after opening the v5 tab.
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const target = process.argv[2], label = process.argv[3] || 'run', extra = process.argv.slice(4);
  const url = /^https?:|^file:/.test(target) ? target : 'file:///' + path.resolve(target).replace(/\\/g, '/');
  const opts = { args: extra }; if (process.env.PW_CHANNEL) opts.channel = process.env.PW_CHANNEL;
  const b = await chromium.launch(opts);
  const ctx = await b.newContext({ viewport: { width: 1280, height: 900 } });
  const p = await ctx.newPage();
  const errs = []; p.on('pageerror', (e) => errs.push('pageerror: ' + e.message)); p.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()); });
  await p.addInitScript(() => {
    window.__lt = []; window.__prog = []; window.__calls = {};
    try { new PerformanceObserver((l) => l.getEntries().forEach((e) => __lt.push([Math.round(e.startTime), Math.round(e.duration)]))).observe({ type: 'longtask', buffered: true }); } catch (e) {}
    const tag = (s) => { const k = []; for (const [w, n] of [['uDesign', 'coin'], ['liq(', 'bubble3d'], ['void trav', 'gem3d'], ['uBackOn', 'medal3d'], ['uShape', 'gem2d'], ['uLqRefr', 'bubble'], ['ageLayers', '+age']]) if (s.indexOf(w) >= 0) k.push(n); return k.join(' ') + ' ' + s.length + 'ch'; };
    for (const P of [WebGLRenderingContext.prototype, window.WebGL2RenderingContext && WebGL2RenderingContext.prototype].filter(Boolean)) {
      const ss = P.shaderSource, at = P.attachShader, lp = P.linkProgram, gp = P.getProgramParameter, up = P.useProgram, da = P.drawArrays;
      window.__first = {}; let usedTag = '';
      P.useProgram = function (pr) { usedTag = pr && pr.__tag || ''; return up.apply(this, arguments); };
      P.drawArrays = function () { const k = usedTag.split(' ').slice(0, 2).join(' '); if (!(k in __first)) __first[k] = Math.round(performance.now()); return da.apply(this, arguments); };
      P.shaderSource = function (sh, src) { sh.__src = src; return ss.apply(this, arguments); };
      P.attachShader = function (pr, sh) { if (sh.__src && /gl_FragColor|out vec4/.test(sh.__src)) pr.__tag = tag(sh.__src); return at.apply(this, arguments); };
      P.linkProgram = function (pr) { pr.__t = performance.now(); return lp.apply(this, arguments); };
      P.getProgramParameter = function (pr, n) {
        const t = performance.now(); const r = gp.apply(this, arguments); const d = performance.now() - t;
        const k = n === this.LINK_STATUS ? 'link' : n === 37297 ? 'poll' : 'other';
        (__calls[k] = __calls[k] || [0, 0])[0]++; __calls[k][1] += d;
        if (k === 'link' && !pr.__done) { pr.__done = 1; __prog.push({ tag: pr.__tag, blockedMs: Math.round(d), sinceLinkMs: Math.round(performance.now() - pr.__t), at: Math.round(performance.now()) }); }
        return r;
      };
    }
  });
  const t0 = Date.now();
  await p.goto(url, { waitUntil: 'load' });
  const loadMs = Date.now() - t0;
  await p.waitForTimeout(10000);
  const snap = () => p.evaluate(() => ({ first: Object.assign({}, __first), lt: __lt.slice(), prog: __prog.slice(), calls: JSON.parse(JSON.stringify(__calls)), now: Math.round(performance.now()),
    renderer: (() => { try { const g = document.createElement('canvas').getContext('webgl'); const e = g.getExtension('WEBGL_debug_renderer_info'); return e ? g.getParameter(e.UNMASKED_RENDERER_WEBGL) : g.getParameter(g.RENDERER); } catch (e) { return '?'; } })(),
    par: (() => { try { return !!document.createElement('canvas').getContext('webgl').getExtension('KHR_parallel_shader_compile'); } catch (e) { return false; } })() }));
  const a = await snap();
  const sum = (l, from, to) => { const x = l.filter((e) => e[0] >= from && e[0] < to); return { n: x.length, total: x.reduce((s, e) => s + e[1], 0), max: Math.max(0, ...x.map((e) => e[1])) }; };
  const out = { label, renderer: a.renderer, parallelCompile: a.par, loadMs, first10s: sum(a.lt, 0, 1e9), first3s: sum(a.lt, 0, 3000), programsOnLoad: a.prog, glCalls: a.calls };
  // then open the medal grid (v5) and measure the next 8 s
  const tClick = a.now;
  await p.click('#vt-v5').catch(() => {});
  await p.waitForTimeout(20000);
  const c = await snap();
  out.firstDrawMs = {}; for (const k in c.first) out.firstDrawMs[k] = c.first[k] - (c.first[k] >= tClick ? tClick : 0) + (c.first[k] >= tClick ? ' after v5 click' : ' after load');
  out.v5Open = sum(c.lt, tClick, 1e9);
  out.programsAfterV5 = c.prog.slice(a.prog.length);
  out.errors = errs.slice(0, 8);
  console.log('PERF ' + JSON.stringify(out));
  await b.close();
})();
