// Renders the app icons and installer art from pulse.js.
//   node tools/icon/make.js        (needs Playwright + Chromium)
// Writes res/app.ico, res/app_light.ico, res/app.png and res/wizard*.bmp.
const fs = require('fs'), path = require('path');
const { chromium } = require('playwright');
const RES = path.join(__dirname, '..', '..', 'res');
const SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

function bmpEntry(S, rgba) { // 32-bit DIB + AND mask, rows bottom-up
  const hdr = Buffer.alloc(40), row = Math.ceil(S / 32) * 4, mask = Buffer.alloc(row * S), px = Buffer.alloc(S * S * 4);
  hdr.writeUInt32LE(40, 0); hdr.writeInt32LE(S, 4); hdr.writeInt32LE(S * 2, 8); hdr.writeUInt16LE(1, 12); hdr.writeUInt16LE(32, 14);
  hdr.writeUInt32LE(px.length + mask.length, 20);
  for (let y = 0; y < S; y++) for (let x = 0; x < S; x++) {
    const s = (y * S + x) * 4, d = ((S - 1 - y) * S + x) * 4;
    px[d] = rgba[s + 2]; px[d + 1] = rgba[s + 1]; px[d + 2] = rgba[s]; px[d + 3] = rgba[s + 3];
  }
  return Buffer.concat([hdr, px, mask]);
}
function ico(entries) { // [{S, data}]
  const head = Buffer.alloc(6 + 16 * entries.length);
  head.writeUInt16LE(0, 0); head.writeUInt16LE(1, 2); head.writeUInt16LE(entries.length, 4);
  let off = head.length;
  entries.forEach((e, i) => {
    const o = 6 + i * 16;
    head[o] = e.S >= 256 ? 0 : e.S; head[o + 1] = e.S >= 256 ? 0 : e.S;
    head.writeUInt16LE(1, o + 4); head.writeUInt16LE(32, o + 6);
    head.writeUInt32LE(e.data.length, o + 8); head.writeUInt32LE(off, o + 12); off += e.data.length;
  });
  return Buffer.concat([head, ...entries.map(e => e.data)]);
}
function bmp24(W, H, rgba) {
  const row = Math.ceil(W * 3 / 4) * 4, size = 54 + row * H, b = Buffer.alloc(size);
  b.write('BM', 0); b.writeUInt32LE(size, 2); b.writeUInt32LE(54, 10); b.writeUInt32LE(40, 14);
  b.writeInt32LE(W, 18); b.writeInt32LE(H, 22); b.writeUInt16LE(1, 26); b.writeUInt16LE(24, 28); b.writeUInt32LE(row * H, 34);
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
    const s = (y * W + x) * 4, d = 54 + (H - 1 - y) * row + x * 3;
    b[d] = rgba[s + 2]; b[d + 1] = rgba[s + 1]; b[d + 2] = rgba[s];
  }
  return b;
}

(async () => {
  const b = await chromium.launch(), p = await b.newPage();
  await p.setContent('<!doctype html><body></body>');
  await p.addScriptTag({ content: fs.readFileSync(path.join(__dirname, 'pulse.js'), 'utf8') });
  const out = await p.evaluate(({ SIZES }) => {
    const icon = (S, look) => { const c = document.createElement('canvas'); c.width = c.height = S; drawPulse(c.getContext('2d'), S, look); return c; };
    const raw = c => Array.from(c.getContext('2d').getImageData(0, 0, c.width, c.height).data);
    const r = { icons: {}, png: icon(256, 'dark').toDataURL('image/png').split(',')[1], art: {} };
    for (const look of ['dark', 'light']) r.icons[look] = SIZES.map(S => ({ S, px: S === 256 ? icon(S, look).toDataURL('image/png').split(',')[1] : raw(icon(S, look)) }));
    // Installer side panel (WizardImageFile) and corner logo (WizardSmallImageFile), 100% and 200%.
    const side = (k) => {
      const W = 164 * k, H = 314 * k, c = document.createElement('canvas'); c.width = W; c.height = H; const x = c.getContext('2d');
      const g = x.createLinearGradient(0, 0, 0, H); g.addColorStop(0, '#1E1E20'); g.addColorStop(1, '#121213'); x.fillStyle = g; x.fillRect(0, 0, W, H);
      const S = 80 * k; x.save(); x.translate((W - S) / 2, H * 0.3); drawPulse(x, S, 'dark'); x.restore();
      x.textAlign = 'center'; x.fillStyle = '#F2F2F2'; x.font = `600 ${15 * k}px "Segoe UI", Inter, sans-serif`;
      x.fillText('Resource Monitor', W / 2, H * 0.3 + S + 30 * k);
      x.fillStyle = '#969696'; x.font = `500 ${9 * k}px "Segoe UI", Inter, sans-serif`;
      if ('letterSpacing' in x) x.letterSpacing = `${2 * k}px`;
      x.fillText('EMULSION LABS', W / 2, H * 0.3 + S + 50 * k);
      return { W, H, px: raw(c) };
    };
    const small = (k) => {
      const W = 55 * k, c = document.createElement('canvas'); c.width = c.height = W; const x = c.getContext('2d');
      x.fillStyle = '#FFFFFF'; x.fillRect(0, 0, W, W); const S = 48 * k; x.translate((W - S) / 2, (W - S) / 2); drawPulse(x, S, 'dark');
      return { W, H: W, px: raw(c) };
    };
    r.art = { 'wizard.bmp': side(1), 'wizard@2x.bmp': side(2), 'wizard-small.bmp': small(1), 'wizard-small@2x.bmp': small(2) };
    return r;
  }, { SIZES });
  await b.close();
  for (const [look, file] of [['dark', 'app.ico'], ['light', 'app_light.ico']]) {
    const entries = out.icons[look].map(e => ({ S: e.S, data: e.S === 256 ? Buffer.from(e.px, 'base64') : bmpEntry(e.S, e.px) }));
    fs.writeFileSync(path.join(RES, file), ico(entries));
  }
  fs.writeFileSync(path.join(RES, 'app.png'), Buffer.from(out.png, 'base64'));
  for (const [f, a] of Object.entries(out.art)) fs.writeFileSync(path.join(RES, f), bmp24(a.W, a.H, a.px));
  console.log('wrote', fs.readdirSync(RES).join(', '));
})();
