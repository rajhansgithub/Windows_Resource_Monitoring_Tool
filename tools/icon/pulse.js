// Resource Monitor mark ("Pulse Line"). Same geometry as src/logo.rs.
// Used by make.js to render res/app.ico, res/app_light.ico and the installer art.
const LOOK = {
  dark: { cpu: '#4FC3F7', ram: '#B39DDB', gpu: '#FF8A65', tile: ['#2C2C2E', '#151516'], edge: 'rgba(255,255,255,.09)', grid: 'rgba(255,255,255,.05)' },
  // Light accents follow the app's rule: metric colour mixed 38% toward black.
  light: { cpu: '#317999', ram: '#6F6188', gpu: '#9E563F', tile: ['#FBF8F1', '#E7E1D3'], edge: 'rgba(42,38,32,.16)', grid: 'rgba(42,38,32,.07)' },
};
// Hand-set line weight per size (device px): thin enough to stay crisp at tray size.
function weight(S) {
  if (S <= 16) return 1.25; if (S <= 20) return 1.5; if (S <= 24) return 1.6;
  if (S <= 32) return 2; if (S <= 48) return 2.75; if (S <= 64) return 3.25; return S * 0.04;
}
const PTS = [[0.14, 0.56], [0.36, 0.56], [0.43, 0.38], [0.52, 0.76], [0.61, 0.24], [0.69, 0.56], [0.86, 0.56]];
const PTS_S = [[0.12, 0.56], [0.36, 0.56], [0.44, 0.36], [0.53, 0.76], [0.63, 0.22], [0.71, 0.56], [0.88, 0.56]];

function drawPulse(x, S, look) {
  const P = LOOK[look], r = S * 0.22, small = S <= 32;
  x.beginPath(); x.roundRect(0, 0, S, S, r);
  const g0 = x.createLinearGradient(0, 0, 0, S); g0.addColorStop(0, P.tile[0]); g0.addColorStop(1, P.tile[1]);
  x.fillStyle = g0; x.fill();
  const e = S >= 32 ? Math.max(1, S / 96) : 1;
  x.strokeStyle = P.edge; x.lineWidth = e; x.beginPath(); x.roundRect(e / 2, e / 2, S - e, S - e, r - e / 2); x.stroke();
  if (!small) {
    x.strokeStyle = P.grid; x.lineWidth = Math.max(1, S / 128);
    for (let i = 1; i < 4; i++) { const y = S * (0.2 + i * 0.18); x.beginPath(); x.moveTo(S * 0.12, y); x.lineTo(S * 0.88, y); x.stroke(); }
  }
  const snap = v => small ? Math.round(v - 0.5) + 0.5 : v;
  const pts = (small ? PTS_S : PTS).map(p => [snap(p[0] * S), snap(p[1] * S)]);
  const g = x.createLinearGradient(pts[0][0], 0, pts[pts.length - 1][0], 0);
  g.addColorStop(0, P.cpu); g.addColorStop(0.5, P.ram); g.addColorStop(1, P.gpu);
  x.lineJoin = 'round'; x.lineCap = 'round'; x.lineWidth = weight(S); x.strokeStyle = g;
  x.beginPath(); x.moveTo(pts[0][0], pts[0][1]); pts.slice(1).forEach(p => x.lineTo(p[0], p[1])); x.stroke();
}
if (typeof module !== 'undefined') module.exports = { drawPulse };
