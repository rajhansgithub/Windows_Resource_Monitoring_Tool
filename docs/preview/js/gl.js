/* One WebGL context for the whole page (badges, Emulsion bubbles, the 3D view, the coin cards), and the shader programs.
   - Programs compile on demand, one variant per feature set (#defines), with KHR_parallel_shader_compile when the browser has it.
     Their status is never asked in the frame they were linked: a small ticker polls on later frames and calls back when ready.
     Without the extension, at most one program is finished per frame (that query blocks while the driver compiles).
   - Draw budget: every engine asks left(now) before each canvas; the page spends at most BUDGET ms of drawing per frame.
   - The shared canvas is sized per draw; out(target) hands the frame to a visible canvas (ImageBitmap when it can, else a copy). */
(function (root) {
  'use strict';
  const VS = 'attribute vec2 a;void main(){gl_Position=vec4(a,0.,1.);}';
  const BUDGET = 9;
  let gl = null, cv = null, off = false, par = null, broken = false, frame = 0, ticking = false, used = null;
  const progs = new Map(), lostCbs = [];
  let fNow = -1, fStart = 0;

  function ctx() {
    if (gl && !gl.isContextLost()) return gl;
    if (broken) return null;
    progs.clear(); used = null;
    try {
      off = typeof root.OffscreenCanvas === 'function' && typeof root.ImageBitmapRenderingContext === 'function';
      cv = off ? new root.OffscreenCanvas(64, 64) : document.createElement('canvas');
      gl = cv.getContext('webgl', { premultipliedAlpha: true, antialias: false, alpha: true });
      if (!gl && off) { off = false; cv = document.createElement('canvas'); gl = cv.getContext('webgl', { premultipliedAlpha: true, antialias: false, alpha: true }); }
    } catch (e) { gl = null; }
    if (!gl) { broken = true; return null; }
    cv.addEventListener('webglcontextlost', (ev) => { ev.preventDefault(); progs.clear(); used = null; lostCbs.forEach((f) => { try { f(); } catch (e) { /* ignore */ } }); });
    cv.addEventListener('webglcontextrestored', () => { gl = null; ctx(); });
    par = gl.getExtension('KHR_parallel_shader_compile');
    const buf = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    return gl;
  }

  /** Program `key` from fragment source `fs` (a function, called only on first use) with `defs` (#define lines) after the precision line.
      Returns the record; usable when rec.ok. `names` = uniforms to look up when it is ready. */
  function program(key, fs, defs, names, onReady) {
    let x = progs.get(key);
    if (x) { if (onReady && !x.done) x.cbs.push(onReady); return x; }
    const g = ctx();
    if (!g) return null;
    let src = typeof fs === 'function' ? fs() : fs;
    if (defs) src = src.replace('precision highp float;', 'precision highp float;\n' + defs);
    const mk = (type, s) => { const o = g.createShader(type); g.shaderSource(o, s); g.compileShader(o); return o; };
    const v = mk(g.VERTEX_SHADER, VS), f = mk(g.FRAGMENT_SHADER, src);
    const p = g.createProgram(); g.attachShader(p, v); g.attachShader(p, f); g.bindAttribLocation(p, 0, 'a'); g.linkProgram(p);
    x = { key, p, v, f, names: names || [], born: frame, done: false, ok: false, U: {}, cbs: onReady ? [onReady] : [] };
    progs.set(key, x);
    tick();
    return x;
  }
  function finish(x) {
    x.done = true;
    x.ok = !!gl.getProgramParameter(x.p, gl.LINK_STATUS);
    if (!x.ok) console.info('[RM] shader ' + x.key + ':', gl.getShaderInfoLog(x.f) || gl.getProgramInfoLog(x.p));
    else for (const k of x.names) x.U[k] = gl.getUniformLocation(x.p, k);
    const cbs = x.cbs; x.cbs = [];
    cbs.forEach((f) => { try { f(x); } catch (e) { console.error(e); } });
  }
  /** The ticker: runs only while programs are compiling. */
  function tick() {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(function step() {
      frame++;
      let waiting = 0, blocked = false;
      for (const x of progs.values()) {
        if (x.done) continue;
        waiting++;
        if (x.born >= frame) continue;                                   // linked this frame: ask next frame
        if (par) { if (gl.getProgramParameter(x.p, par.COMPLETION_STATUS_KHR)) { finish(x); waiting--; } }
        else if (!blocked) { blocked = true; finish(x); waiting--; }      // no parallel compile: one per frame
      }
      if (waiting && gl) requestAnimationFrame(step); else ticking = false;
    });
  }
  /** Resolves when the program is ready (true) or failed (false). */
  const when = (x) => new Promise((res) => { if (!x) return res(false); if (x.done) return res(x.ok); x.cbs.push((y) => res(y.ok)); });
  function use(x) { if (used !== x.p) { gl.useProgram(x.p); used = x.p; } }
  /** Size the shared canvas for one draw and set the viewport. */
  function size(w, h) { if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; } gl.viewport(0, 0, w, h); }
  /** Hand the drawn frame to `target` (a visible canvas). Each target keeps one context kind. */
  function out(target) {
    if (off) {
      const c = target.__bm || (target.__bm = target.getContext('bitmaprenderer'));
      if (c) { c.transferFromImageBitmap(cv.transferToImageBitmap()); return; }
    }
    const c = target.__2d || (target.__2d = target.getContext('2d'));
    if (!c) return;
    c.clearRect(0, 0, target.width, target.height); c.drawImage(cv, 0, 0, target.width, target.height);
  }
  /** Milliseconds of drawing left in this frame (all engines share it). */
  function left(now) {
    if (now !== fNow) { fNow = now; fStart = performance.now(); }
    return BUDGET - (performance.now() - fStart);
  }
  root.YTPMGL = {
    ctx, program, when, use, size, out, left,
    canvas: () => cv, offscreen: () => off, parallel: () => !!par,
    onLost: (f) => lostCbs.push(f),
    idle: (f, t) => (root.requestIdleCallback ? root.requestIdleCallback(f, { timeout: t || 800 }) : setTimeout(f, 120))
  };
})(typeof window !== 'undefined' ? window : globalThis);
