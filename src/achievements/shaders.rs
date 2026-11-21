//! HLSL source text for the achievement badges (shader model 5, compiled at run time by `gpu.rs`).
//!
//! Ported line by line from the `achievements` skill reference (`reference/bubble.js` and `reference/gem.js`).
//! Every look number (bubble look params, metal reflectances, gem ior / dispersion / cut, studio lights) is kept
//! exactly. Port notes (GLSL to HLSL):
//!   vec -> float, mix -> lerp, fract -> frac, atan(y,x) -> atan2(y,x), mod -> `modg` (x - y*floor(x/y)),
//!   sign -> `sg` (HLSL sign() returns int), refract -> `refr` (the GLSL formula, zero on total internal reflection),
//!   pow(negative, 2.) -> x*x (HLSL pow of a negative base is NaN, GLSL "undefined but works"),
//!   `mat2 M; M*v` -> `rotv` / `fbm` written out (GLSL is column-major: rot(a)*v = (c*x+s*y, -s*x+c*y);
//!   `v*rot(a)` = (c*x-s*y, s*x+c*y) = `rotT`),
//!   smoothstep with reversed edges -> `sstep` (same clamp-and-cubic formula, defined for e0 > e1),
//!   gl_FragCoord (y up, origin bottom-left of the item) -> rebuilt from SV_Position and the item's atlas offset,
//!   texture2D(...).a on a white-on-clear mask -> R8 texture, `.r`, sampled with SampleLevel and an analytic LOD
//!   (no screen-space derivatives, so every call may sit inside branches).
//! Shared by both pixel shaders: the constant buffer `C` (one per item) and the helpers in `COMMON`.

/// Full-screen triangle (clockwise), no vertex buffer. Entry point `vsmain`.
pub const VS: &str = r#"
float4 vsmain(uint id : SV_VertexID) : SV_Position {
  return float4((id == 2u) ? 3. : -1., (id == 1u) ? 3. : -1., 0., 1.);
}
"#;

/// Constant buffer, helpers and noise shared by both pixel shaders. The Rust side (`CbData` in gpu.rs) mirrors
/// the buffer layout: 5 rows of 4 floats.
pub const COMMON: &str = r#"
cbuffer C : register(b0) {
  float2 uRes;  float2 uOff;               // item size in px (square), item origin in the atlas (top-left, px)
  float  uTime; float  uSeed; float uKind; float uShape;
  float3 uMetal; float uPx;                // metal colour, item size in px
  float2 uTilt; float uIconOn; float uDetOn;
  float  uDetN; float uP0; float uP1; float uP2;   // detail mask side in texels
};
SamplerState smp : register(s0);
Texture2D<float> uIcon : register(t0);     // 128x128 alpha (white-on-clear icon), mip chain
Texture2D<float> uDet  : register(t1);     // NxN alpha (etched text), mip chain
static const float PI  = 3.14159265;
static const float TAU = 6.2831853;
static float2 gFrag = float2(0., 0.);      // gl_FragCoord.xy of the reference (y up inside the item)

float sstep(float e0, float e1, float x){ float t = saturate((x - e0) / (e1 - e0)); return t*t*(3. - 2.*t); }
#define smoothstep sstep
float  sg(float x){ return x > 0. ? 1. : (x < 0. ? -1. : 0.); }
float2 sg2(float2 x){ return float2(sg(x.x), sg(x.y)); }
float3 v3(float x){ return float3(x, x, x); }
float  modg(float x, float y){ return x - y*floor(x/y); }
float2 rotv(float a, float2 v){ float c = cos(a), s = sin(a); return float2(c*v.x + s*v.y, -s*v.x + c*v.y); }   // rot(a)*v
float2 rotT(float a, float2 v){ float c = cos(a), s = sin(a); return float2(c*v.x - s*v.y,  s*v.x + c*v.y); }   // v*rot(a)
float3 refr(float3 I, float3 N, float eta){
  float k = 1. - eta*eta*(1. - dot(N, I)*dot(N, I));
  return k < 0. ? float3(0., 0., 0.) : eta*I - (eta*dot(N, I) + sqrt(k))*N;
}
float hash(float2 p){ p = frac(p*float2(123.34, 456.21)); p += dot(p, p + 45.32); return frac(p.x*p.y); }
float vnoise(float2 p){
  float2 i = floor(p), f = frac(p); float2 u = f*f*(3. - 2.*f);
  return lerp(lerp(hash(i), hash(i + float2(1., 0.)), u.x), lerp(hash(i + float2(0., 1.)), hash(i + float2(1., 1.)), u.x), u.y);
}
float sdRBox(float2 p, float2 b, float r){ float2 q = abs(p) - b + r; return length(max(q, float2(0., 0.))) + min(max(q.x, q.y), 0.) - r; }
float seg(float2 p, float2 a, float2 b){ float2 pa = p - a, ba = b - a; float h = clamp(dot(pa, ba)/dot(ba, ba), 0., 1.); return length(pa - ba*h); }
"#;

/// Emulsion: the soap bubble (bubble.js), with the Resource Monitor mark in the middle.
/// Not ported: the optional reflection of the app behind (uEnv, RK) and the see-through alpha (uSee = 0 there too
/// when no reflection is asked for), so alpha = `inside`.
pub const BUBBLE: &str = r#"
#define uEdgeW 0.59
#define uEdgeVary 0.85
#define uEdgeCol 1.13
#define uSat 0.8
#define uFilmIn 1.0
#define uRefl 0.6
#define uFlow 2.0
#define uThick 497.0
#define uRim 0.3
#define uDotRefr 0.3
#define uDotRing 0.3
#define uRefrT 20.0
#define uGlow 0.43
#define uBreath 15.0
#define uMotAmt 1.0
#define uMotT 10.5
#define uVar 2.0

float fbm(float2 p){
  float v = 0., a = .5;
  for (int i = 0; i < 5; i++){ v += a*vnoise(p); p = float2(1.6*p.x - 1.2*p.y, 1.2*p.x + 1.6*p.y); a *= .5; }   // m = mat2(1.6,1.2,-1.2,1.6)
  return v;
}
float3 wl(float l){
  float a = (l - 605.)/48., b = (l - 425.)/28., c = (l - 545.)/42., d = (l - 455.)/32.;
  return float3(exp(-a*a) + .28*exp(-b*b), exp(-c*c), exp(-d*d));
}
float3 film(float d, float cosT){
  float3 c = float3(0., 0., 0.), s = float3(0., 0., 0.);
  for (int i = 0; i < 8; i++){
    float l = 400. + (float)i*42.857; float3 w = wl(l);
    float ph = 6.2831853*2.*1.33*d*cosT/l; c += w*(.5 - .5*cos(ph)); s += w;
  }
  float3 f = 2.*c/s;
  float y = dot(f, float3(.299, .587, .114));
  return lerp(v3(y), f, uSat);
}
// Resource Monitor mark (24-unit box): monitor outline, stand, and a pulse line; breathes very slightly.
float4 logo(float2 q, float aa){
  float cs = 1. + uMotAmt*.05*sin(uTime*TAU/uMotT);
  q /= cs;
  float2 p = float2(12. + q.x*12., 11.8 - q.y*12.);
  float a = aa*12.;
  float scr = abs(sdRBox(p - float2(12., 9.6), float2(10., 7.), 1.5)) - .85;
  float st = min(seg(p, float2(12., 16.8), float2(12., 19.8)), seg(p, float2(8.4, 20.6), float2(15.6, 20.6))) - .85;
  float pl = min(min(min(seg(p, float2(5.6, 9.8), float2(8., 9.8)), seg(p, float2(8., 9.8), float2(9.6, 6.4))), min(seg(p, float2(9.6, 6.4), float2(11.3, 13.)), seg(p, float2(11.3, 13.), float2(13., 5.6)))), min(seg(p, float2(13., 5.6), float2(14.6, 9.8)), seg(p, float2(14.6, 9.8), float2(18.4, 9.8)))) - .75;
  float m = smoothstep(a, -a, min(min(scr, st), pl));
  return float4(m, m, m, m);
}
float3 glow(float2 q, float k){
  float3 g = float3(0., 0., 0.);
  g += float3(.18, .45, 1.)*exp(-dot((q - float2(0., .32))*float2(1.3, 2.4), (q - float2(0., .32))*float2(1.3, 2.4)))*.75;
  g += float3(.15, .77, 1.)*exp(-dot((q - float2(.28, .2))*float2(2., 3.), (q - float2(.28, .2))*float2(2., 3.)))*.5;
  g += float3(.7, .3, 1.)*exp(-dot((q - float2(-.42, -.02))*float2(2.4, 3.6), (q - float2(-.42, -.02))*float2(2.4, 3.6)))*.45;
  g += v3(1.)*exp(-dot(q*float2(1.8, 3.2), q*float2(1.8, 3.2)))*.8;
  g += float3(1., .82, .25)*exp(-dot((q - float2(0., -.3))*float2(1.4, 2.6), (q - float2(0., -.3))*float2(1.4, 2.6)))*.7;
  g += float3(1., .36, .16)*exp(-dot((q - float2(.45, -.2))*float2(2.4, 3.4), (q - float2(.45, -.2))*float2(2.4, 3.4)))*.55;
  return g*k*uGlow;
}
float3 content(float2 q, float k, float aa){
  float4 L = logo(q, aa);
  float3 dotc = v3(.95);
  float3 g = glow(q, k);
  if (uVar > 1.5 && uVar < 2.5){
    float y = q.y; dotc = lerp(float3(1., .42, .23), float3(1., .86, .45), smoothstep(-.9, -.3, y));
    dotc = lerp(dotc, v3(1.), smoothstep(-.25, 0., y)); dotc = lerp(dotc, float3(.55, .85, 1.), smoothstep(.1, .45, y)); dotc = lerp(dotc, float3(.31, .55, 1.), smoothstep(.5, .95, y));
    g *= .55;
  } else if (uVar > 2.5){
    g = (v3(1.)*exp(-dot(q, q)*9.)*1.1 + float3(1., .82, .3)*exp(-dot(q, q)*3.5)*.55 + float3(.2, .7, 1.)*exp(-dot(q, q)*1.4)*.25)*k*uGlow;
    dotc = v3(1.)*(.45 + .55*exp(-dot(q, q)*1.6));
  }
  return g + L.a*dotc*(1. - .15*k);
}

float4 main(float4 sv : SV_Position) : SV_Target {
  gFrag = float2(sv.x - uOff.x, uRes.y - (sv.y - uOff.y));
  float S = min(uRes.x, uRes.y);
  float2 p = (gFrag - .5*uRes)/(.5*S);
  float r = length(p);
  float edgeAA = 2./S;
  float inside = smoothstep(1. + edgeAA, 1. - edgeAA, r);
  if (inside <= 0.) return float4(0., 0., 0., 0.);
  float t = uTime + uSeed;
  float ft = t*uFlow;
  float rc = min(r, .9999);
  float z = sqrt(1. - rc*rc);
  float F = .02 + .98*pow(1. - z, 5.);
  float sinT = rc/1.33; float cosT = sqrt(1. - sinT*sinT);
  float k = .62 + .38*sin(t*TAU/uBreath);

  // edge band: width varies around the outline (hairline here, broad there), slowly shifting
  float ang = atan2(p.y, p.x);
  float2 cp = float2(cos(ang), sin(ang));
  float bw = fbm(cp*1.7 + float2(ft*.035, -ft*.028)) + .35*fbm(cp*4.1 - float2(ft*.05, ft*.04));
  float wf = lerp(1., lerp(.14, 3.2, smoothstep(.45, .95, bw)), uEdgeVary)*uEdgeW;
  float rB = clamp(1. - (1. - r)/max(wf, .01), 0., .9999);
  float FB = .02 + .98*pow(1. - sqrt(1. - rB*rB), 5.);

  // film thickness: smooth, large swirls
  float2 q = p*1.2;
  q = rotv(.04*ft, q);
  float2 w = float2(fbm(q*.8 + float2(0., ft*.05)), fbm(q*.8 + float2(5.2, 1.3) - ft*.04));
  float n = fbm(q + 1.3*w + float2(ft*.02, -ft*.015));
  float d = uThick + 420.*(n - .5) - 120.*p.y;
  float3 fc = film(max(d, 20.), cosT);
  float2 wp = rotv(.5, p - float2(-.36, .42));
  float win = smoothstep(.45, .05, length(max(abs(wp) - float2(.2, .1), float2(0., 0.))));
  float floorr = smoothstep(-.2, -.95, p.y)*.6;
  float env = .25 + uRefl*(win + floorr);
  float filmI = (uFilmIn*.05 + uEdgeCol*2.6*FB)*env;
  float3 col = fc*filmI;

  float aa = 1.6/(S*.5*.62);
  col += content(p/.62, k, aa)*(1. - F);

  float sway = .26*sin(t*TAU/uRefrT) + .1*sin(t*TAU/(uRefrT*1.45));
  if (uDotRing > 0.){
    float wr = smoothstep(.8, .88, r)*smoothstep(1., .96, r);
    if (wr > 0.){
      float s = (r - .8)/.195; float rr = (1. - s)*1.02;
      float2 dir = rotT(sway, -normalize(p));
      float aa2 = aa*3.;
      float cr = content(dir*rr*1.05, k, aa2).r, cg = content(dir*rr, k, aa2).g, cb = content(dir*rr*.95, k, aa2).b;
      col += float3(cr, cg, cb)*wr*.3*uDotRing*(.75 + .25*k);
    }
  }
  if (uDotRefr > 0.){
    [unroll] for (int j = 0; j < 2; j++){
      float side = (j == 0) ? -1. : 1.;
      float a2 = ((j == 0) ? 3.1416 : 0.) + side*(.16*sin(t*TAU/uRefrT + (float)j*1.7) + .06*sin(t*TAU/(uRefrT*1.41))) - .1;
      float2 nrm = float2(cos(a2), sin(a2)); float2 tng = float2(-nrm.y, nrm.x);
      float2 u = p - nrm*.8; float2 lu = float2(dot(u, tng), dot(u, nrm)*1.9);
      float sz = .19; float2 qq = -lu/sz;
      float m = smoothstep(1.08, .8, length(qq))*smoothstep(.995, .94, r);
      if (m > 0.){
        float aa3 = aa*.62/sz*2.;
        float cr = content(qq*1.06, k, aa3).r, cg = content(qq, k, aa3).g, cb = content(qq*.94, k, aa3).b;
        col += float3(cr, cg, cb)*m*uDotRefr*(.7 + .3*k);
      }
    }
  }
  col += fc*smoothstep(.985, .997, r)*smoothstep(1.002, .996, r)*uRim;
  col = 1. - exp(-col*1.15);
  return float4(col*inside, inside);
}
"#;

/// Metals, Diamond and Ruby (gem.js), drawn as the Case Badge: a 4:3 landscape rounded rectangle.
/// Reference pieces kept as they are: `envM`, `env`, `inner`, `brill` (radiant cut), `stoneTri`, the pave (carved
/// stone border) maths, the brushed face, the etched text and the tone maps.
/// New for this theme (no reference): the badge outline `sdB`, the raised chrome rim, the domed resin coat, the raised
/// frosted icon. Adapted: `pave` / `caseS` are `pave` / `vplaque` re-fitted to the badge outline.
pub const GEM: &str = r#"
// ---- studio light for stones: soft ceiling, strip lights round the side that turn slowly, hard spots for sparkle,
// and the viewer's own dark head straight above
float env(float3 d, float t){
  d.xy = rotv(t*.23, d.xy);
  float az = atan2(d.y, d.x);
  float s = .03 + .45*smoothstep(-.1, .9, d.z);
  s += 1.1*pow(max(cos(az*3.)*.5 + .5, 0.), 5.)*smoothstep(-.2, .5, d.z)*smoothstep(.98, .75, d.z);
  s += pow(max(dot(d, normalize(float3(.55, .55, .63))), 0.), 60.)*7.;
  s += pow(max(dot(d, normalize(float3(-.72, .25, .64))), 0.), 40.)*4.;
  s += pow(max(dot(d, normalize(float3(.05, -.78, .62))), 0.), 80.)*6.;
  s *= 1. - .95*smoothstep(.94, .985, d.z);
  return s;
}
// light that enters through a facet (normal n), bounces off two pavilion mains and leaves through the crown.
float inner(float2 c, float3 v, float3 n, float ior, float t){
  float3 T = refr(-v, n, 1./ior);
  if (dot(T, T) < .01) return 0.;
  float2 q = c + T.xy*.7;
  float az = atan2(q.y, q.x);
  float ps = (floor(az/(PI/4.)) + .5)*(PI/4.);
  float th = radians(40.8);
  float3 np = float3(cos(ps)*sin(th), sin(ps)*sin(th), -cos(th));
  float3 D1 = reflect(T, np);
  float2 q2 = q + D1.xy*.8;
  float az2 = atan2(q2.y, q2.x);
  float ps2 = (floor(az2/(PI/4.)) + .5)*(PI/4.);
  float3 np2 = float3(cos(ps2)*sin(th), sin(ps2)*sin(th), -cos(th));
  float3 D2 = reflect(D1, np2);
  if (D2.z <= 0.) return .02;
  float3 O = refr(D2, float3(0., 0., -1.), ior);
  if (dot(O, O) < .01) return .05;
  return env(normalize(O), t);
}
// ---- metal light (struck / brushed metals)
float envM(float3 d, float t){
  d.xy = rotv(t*.25, d.xy);
  float s = .10 + .30*smoothstep(-.3, 1., d.z);
  s += 1.5*pow(max(dot(d, normalize(float3(.5, .45, .75))), 0.), 9.);
  s += .7*pow(max(dot(d, normalize(float3(-.65, -.25, .7))), 0.), 14.);
  s += .35*smoothstep(.2, -.4, d.y)*smoothstep(-.6, .3, d.z);
  s += 4.*pow(max(dot(d, normalize(float3(.45, .55, .7))), 0.), 220.);
  return s;
}
float3 tiltv(float c, float a){ return normalize(float3(cos(c)*sin(a), sin(c)*sin(a), cos(a))); }
float segd(float2 p, float2 a, float2 b){ float2 pa = p - a, ba = b - a; float h = clamp(dot(pa, ba)/dot(ba, ba), 0., 1.); return length(pa - ba*h); }

// the radiant-cut stone: the brilliant's triangles and kites drawn into a rectangle with clipped corners (p: |p| = 1 at the girdle)
float4 brill(float2 p, float r, float aa, float t){
  float inside = smoothstep(1. + aa, 1. - aa, r);
  if (inside <= 0.) return float4(0., 0., 0., 0.);
  float ruby = uKind;
  // crown facets (8-fold): local u along the sector's centre, w across
  float a = atan2(p.y, p.x);
  float sec = floor((a + PI/8.)/(PI/4.));
  float ca = sec*PI/4.;
  float s = a - ca;
  float u = r*cos(s), w = r*sin(s);
  float TB = .58, SP = .81, tn = tan(PI/8.);
  float2 lp = float2(u, abs(w));
  float2 tv = float2(TB, TB*tn), st = float2(SP, 0.), g = float2(cos(PI/8.), sin(PI/8.)), g0 = float2(1., 0.);
  float side = sg(w + 1e-6);
  float3 n = float3(0., 0., 1.); float fid = 0.; float2 cen = float2(0., 0.); float ed = 0.;
  bool tbl = u < TB;
  if (tbl){
    // through the table you see the pavilion mains and lower girdles mirrored: 16 wedges, split near the middle
    float wa = floor(a/(PI/8.)); float ring = step(.3 + .04*sin(wa*2.1), r);
    fid = 100. + wa + ring*16.;
    float cwa = (wa + .5)*PI/8.;
    cen = float2(cos(cwa), sin(cwa))*lerp(.18, .45, ring);
    n = tiltv(cwa + PI, lerp(.10, .22, ring));
    float wlin = abs(sin(a*8.))*r;                     // distance to a wedge line
    ed = min(wlin*.9, abs(r - .3 - .04*sin(wa*2.1)));
    ed = min(ed, u - TB < 0. ? TB - u : 1.);
  } else {
    ed = min(min(segd(lp, float2(TB, 0.), tv), segd(lp, tv, st)), min(segd(lp, st, g), segd(lp, st, g0)));
    if (abs(w) < (SP - u)/(SP - TB)*TB*tn){ n = tiltv(ca, .27); fid = 1. + sec; cen = float2(cos(ca), sin(ca))*.65; }
    else {
      float2 e = g - st; float2 l2 = lp - st;
      if (e.x*l2.y - e.y*l2.x < 0.){ float c = ca + side*PI/16.; n = tiltv(c, .74); fid = 20. + sec*2. + side; cen = float2(cos(c), sin(c))*.93; }
      else { float c = ca + side*PI/8.; n = tiltv(c, .6); fid = 40. + modg(sec*2. + (side > 0. ? 1. : 0.) + 16., 16.); cen = float2(cos(c), sin(c))*.8; }
    }
  }
  // the stone rocks a little; each facet is a touch off (real cutting), so they flash at different moments
  float3 v = normalize(float3(.09*sin(t*.47) + uTilt.x*.9, .08*sin(t*.31 + 1.) + uTilt.y*.9, 1.));
  n = normalize(n + float3((hash(float2(fid, 3.1)) - .5)*.08, (hash(float2(fid, 7.7)) - .5)*.08, 0.));
  float ior = lerp(2.417, 1.77, ruby);
  float F0 = pow((ior - 1.)/(ior + 1.), 2.);
  float F = F0 + (1. - F0)*pow(1. - max(dot(n, v), 0.), 5.);
  float outer = env(reflect(-v, n), t);
  // brilliance + fire: three colours, each with its own index (dispersion makes the rainbow flashes)
  float disp = lerp(.06, .02, ruby);
  float2 cp = lerp(cen, p, .35);   // a little of the pixel's own place: soft shading across each facet
  float3 inn = float3(inner(cp, v, n, ior - disp, t), inner(cp, v, n, ior, t), inner(cp, v, n, ior + disp, t));
  float3 col;
  if (ruby < .5){
    col = inn*(1. - F)*1.1 + outer*F*1.5;
    col = pow(col, float3(1.45, 1.45, 1.45))*1.5 + .015;                      // a real brilliant is mostly bright white and near black
  } else {
    // ruby: deep red body; the further light travels inside (towards the rim), the darker and richer it gets
    float lum = dot(inn, float3(.33, .33, .33));
    float path = lerp(.5, 1.2, smoothstep(.15, 1., r));
    float3 absorb = exp(-float3(.05, 3., 2.4)*path);
    col = pow(v3(lum), float3(1.3, 1.3, 1.3))*(1. - F)*float3(1., .1, .17)*absorb*4.2;
    col += float3(.2, .006, .03)*(1. - .5*r);
    // light bounced round the body lights crown facets one by one (a ruby's slow red 'breathing')
    if (!tbl) col += float3(.75, .03, .09)*pow(.5 + .5*sin(t*.8 + hash(float2(fid, 9.))*6.28), 3.)*lerp(1., .45, smoothstep(.8, 1., r));
    col += pow(min(lum, 3.), 2.)*float3(1., .4, .45)*.05;          // the brightest flashes lean pink-white
    col += pow(outer*F, 1.4)*float3(1., .9, .92)*1.6;
  }
  // facet lines: a touch darker with a hairline of light
  float fl = smoothstep(aa*1.3, 0., ed)*smoothstep(.985, .94, r);
  col = lerp(col, col*.6 + lerp(v3(.08), float3(.1, .02, .03), ruby), fl*.55);
  // girdle: thin bright rim
  col += lerp(float3(.6, .62, .68), float3(.7, .2, .26), ruby)*smoothstep(.955, .985, r)*smoothstep(1., .985, r);
  col = max(col, float3(0., 0., 0.));
  col = 1. - exp(-col*1.6);
  return float4(col*inside, inside);
}

// one small triangular stone facet (edge band of the stone badges)
float3 stoneTri(float2 p, float d, float2 g, float t, float ruby, out float ed){
  float cs = .052;
  float2 s = float2(p.x + p.y*.57735, p.y*1.1547)/cs;
  float2 ce = floor(s), f = frac(s);
  float up = step(1., f.x + f.y);
  ed = min(min(f.x, f.y), abs(1. - f.x - f.y))*cs*.8;
  float2 cc = ce + lerp(float2(1./3., 1./3.), float2(2./3., 2./3.), up);
  float2 cen = float2(cc.x - cc.y*.5, cc.y*.866)*cs;
  float fid = hash(ce*.731 + up*float2(.37, .71))*97.;   // per-facet id, no lines of look-alike facets
  float3 v = normalize(float3(.09*sin(t*.47) + uTilt.x*.9, .08*sin(t*.31 + 1.) + uTilt.y*.9, 1.));
  float sl = lerp(.25, .75, smoothstep(-.12, 0., d));                       // the band curves down to the edge
  float3 n = normalize(float3(g*sl + float2(hash(float2(fid, 3.1)) - .5, hash(float2(fid, 7.7)) - .5)*.32, 1.));
  float ior = lerp(2.417, 1.77, ruby);
  float F0 = pow((ior - 1.)/(ior + 1.), 2.);
  float F = F0 + (1. - F0)*pow(1. - max(dot(n, v), 0.), 5.);
  float outer = env(reflect(-v, n), t);
  float disp = lerp(.06, .02, ruby);
  float3 inn = float3(inner(cen, v, n, ior - disp, t), inner(cen, v, n, ior, t), inner(cen, v, n, ior + disp, t));
  float3 col;
  if (ruby < .5){ col = inn*(1. - F)*1.1 + outer*F*1.5; col = pow(col, float3(1.45, 1.45, 1.45))*1.5 + .015; }
  else {
    float lum = dot(inn, float3(.33, .33, .33));
    col = pow(v3(lum), float3(1.3, 1.3, 1.3))*(1. - F)*float3(1., .1, .17)*exp(-float3(.05, 3., 2.4)*.8)*4.2;
    col += float3(.2, .006, .03) + float3(.75, .03, .09)*pow(.5 + .5*sin(t*.8 + hash(float2(fid, 9.))*6.28), 3.)*.6;
    col += pow(outer*F, 1.4)*float3(1., .9, .92)*1.6;
  }
  col = lerp(col, col*.6 + lerp(v3(.08), float3(.1, .02, .03), ruby), smoothstep(.006, 0., ed)*.55);
  return col;
}

// ---- the Case Badge outline: landscape 4:3 rounded rectangle (half size .96 x .72, radius .14)
static const float2 BB = float2(.96, .72);
static const float BRAD = .14;
float sdB(float2 p){ return sdRBox(p, BB, BRAD); }
float2 gradB(float2 p){
  float e = 1.5/max(uPx, 40.);
  return normalize(float2(sdB(p + float2(e, 0.)) - sdB(p - float2(e, 0.)), sdB(p + float2(0., e)) - sdB(p - float2(0., e))) + 1e-6);
}
// icon mask: 128 px square, centred at IC, half size IK (badge units); mip chosen from the on-screen size
static const float2 IC = float2(0., .07);
static const float IK = .38;
float icB(float2 p){
  float2 iu = (p - IC)/IK*.5 + .5;
  if (iu.x < 0. || iu.y < 0. || iu.x > 1. || iu.y > 1.) return 0.;
  float lod = max(log2(128.*1.04/(IK*max(uPx, 8.))), 0.);
  return uIcon.SampleLevel(smp, float2(iu.x, 1. - iu.y), lod);
}
// etched detail (text) mask: whole 2.08 square, same mapping as the reference det(); `dy` moves the picture in badge units
float detA(float2 p, float dy){
  if (uDetOn <= 0.) return 0.;
  float2 u = float2(p.x, p.y - dy)/1.04*.5 + .5;
  if (u.x < 0. || u.y < 0. || u.x > 1. || u.y > 1.) return 0.;
  float lod = max(log2(max(uDetN, 1.)/max(uPx, 8.)), 0.);
  return uDet.SampleLevel(smp, float2(u.x, 1. - u.y), lod);
}

// metal case badge: raised polished chrome rim (bevel normal from the SDF gradient), brushed metal face, raised frosted
// icon, etched text, and a domed clear resin coat over the face.
float4 caseM(float2 p, float aa, float t){
  float3 base = uMetal;
  float d = sdB(p);
  float inside = smoothstep(aa, -aa, d);
  if (inside <= 0.) return float4(0., 0., 0., 0.);
  float3 v = normalize(float3(-p*.3 + uTilt*.9 + float2(.06*sin(t*.5), .05*sin(t*.37 + 1.)), 1.));
  float2 g = gradB(p);
  float W = .13;                                  // rim width
  bool rim = d > -W;
  float3 n;
  if (rim){
    float x = clamp((d + W)/(W*.92), 0., 1.);
    float sl = cos(x*PI)*1.05;                    // rounded raised rim
    sl = lerp(sl, -2.3, smoothstep(-.03, -.008, d));   // and a crisp outer edge
    n = normalize(float3(-g*sl, 1.));
  } else {
    n = normalize(float3((hash(float2(floor(p.y*uPx*.6), 3.)) - .5)*.03, sin(p.y*uPx*1.1)*.02, 1.));   // brushed: fine horizontal lines
  }
  float e = envM(reflect(-v, n), t);
  float3 col;
  if (rim){
    float3 chrome = lerp(base, float3(.90, .92, .95), .7);
    col = chrome*e*1.25 + v3(e*e*.04);
  } else {
    // brushing smears the light into a soft band that slides across the face as the lights turn
    float la = t*.25 + .7 + uTilt.x*2.;
    float dd = dot(p, float2(cos(la), sin(la))) - .35*sin(t*.3) - uTilt.y;
    float sh = exp(-dd*dd*4.);
    col = base*(e*.62 + sh*.38 + .05);
  }
  col *= 1. - smoothstep(aa*1.5, 0., abs(d + W))*.55;   // groove where the rim meets the face
  if (!rim){
    // etched text: frosted (matte, lighter), only where it can be read (big badges)
    float m = detA(p, 0.)*smoothstep(80., 180., uPx);
    if (m > 0.){
      float grain = .85 + .3*hash(floor(gFrag));
      float3 fr = lerp(base, v3(1.), .5)*(.85 + .12*min(envM(float3(.3, .4, 1.), t), 1.2))*grain;
      col = lerp(col, fr, m*.92);
    }
    // the icon, raised: soft shadow, polished walls that catch the light, frosted top
    if (uIconOn > 0.){
      float dd = 1.6/max(uPx, 40.);
      float mi = icB(p);
      float gx = icB(p + float2(dd, 0.)) - icB(p - float2(dd, 0.));
      float gy = icB(p + float2(0., dd)) - icB(p - float2(0., dd));
      float shd = icB(p + float2(-.02, .02));
      col *= 1. - .4*shd*(1. - mi);
      if (mi + abs(gx) + abs(gy) > 0.){
        float grain = .88 + .24*hash(floor(gFrag));
        float3 nw = normalize(float3(-float2(gx, gy)*2.2, 1.));
        float w = clamp(length(float2(gx, gy))*1.6, 0., 1.);
        col = lerp(col, base*envM(reflect(-v, nw), t)*1.3, w*.9);
        float3 top = (base*.7 + .28)*(.8 + .15*min(envM(float3(.2, .5, 1.), t), 1.2))*grain;
        col = lerp(col, top, smoothstep(.15, .85, mi)*.95);
      }
    }
    // domed clear resin coat: a dome normal, an extra Fresnel specular of the studio light, and a window highlight
    float2 pn = p/BB;
    float3 nd = normalize(float3(pn*.7, 1.));
    float3 Rd = reflect(-v, nd);
    float Fd = .07 + .93*pow(1. - max(dot(nd, v), 0.), 5.);
    float cw = smoothstep(-W + aa, -W - aa, d);
    col = col*(1. - Fd*cw) + v3(envM(Rd, t)*Fd*1.4*cw);
    col += v3(pow(max(dot(Rd, normalize(float3(-.35, .5, .8))), 0.), 60.)*.55*cw);
  }
  col = 1. - exp(-max(col, float3(0., 0., 0.))*1.6);
  return float4(col*inside, inside);
}

// ---- stone badges (Diamond / Ruby): vplaque fitted to the badge outline.
static const float PSC = .8;                                        // pave border depth scale (the badge is shorter than the plaque)
static const float PWB = .25;                                       // how far in the stones can reach (.31 * PSC, rounded up)
static const float2 SC = float2(0., .10);                           // stone centre
static const float2 SH = float2(.52, .27);                          // stone half size
static const float SCLIP = .20;                                     // radiant cut: clipped corner size
float paveW(float2 p){ return (.13 + .13*vnoise(p*3.1 + float2(4.2, 1.7)) + .05*vnoise(p*8.3 + float2(9.1, 3.3)))*PSC; }
float paveSdf(float2 p){
  float depth = -sdB(p);
  float ow = .055*vnoise(p*3.3 + float2(2., 7.)) + .025*vnoise(p*9.1 + float2(5., 1.));   // the outside line is wavy too
  return max(ow - depth, depth - paveW(p));
}
// the border is one carved piece of diamond / ruby, cut all over in small triangular facets, domed so the facets tip towards its wavy edges
float4 pave(float2 p, float aa, float t){
  float sd = paveSdf(p);
  if (sd > aa) return float4(0., 0., 0., 0.);
  float e = 1.5/max(uPx, 40.);
  float2 g = normalize(float2(paveSdf(p + float2(e, 0.)) - sd, paveSdf(p + float2(0., e)) - sd) + 1e-6);
  float ed;
  float3 col = stoneTri(p, -.02, g*smoothstep(-.06, 0., sd)*1.4, t, uKind, ed);   // one piece: flat top, rounded only at its own edge
  col += lerp(float3(.5, .52, .58), float3(.6, .15, .2), uKind)*smoothstep(-.012, -.002, sd);   // a bright line along the polished edge
  col = 1. - exp(-max(col, float3(0., 0., 0.))*lerp(2.6, 1.6, uKind));   // diamond a little brighter
  float a = smoothstep(aa, -aa, sd);
  return float4(col*a, a);
}
float4 caseS(float2 p, float aa, float t){
  float ruby = uKind;
  float3 base = float3(.13, .13, .15);               // gunmetal plate
  float dO = sdB(p);
  if (dO > aa) return float4(0., 0., 0., 0.);
  float d = dO + .1;                                  // the plate sits inside, the stones make the outline
  float inside = smoothstep(aa, -aa, d);
  float3 v = normalize(float3(-p*.3 + uTilt*.9 + float2(.05*sin(t*.5), .04*sin(t*.37 + 1.)), 1.));
  float4 pv = float4(0., 0., 0., 0.);
  if (dO > -PWB) pv = pave(p, aa, t);
  if (pv.a >= .999) return pv;
  float2 g = gradB(p);
  float W = .035;
  float3 col; bool edge = d > -W;
  {
    float3 n;
    if (edge) n = normalize(float3(g*1.6, 1.));
    else n = normalize(float3((hash(float2(floor(p.y*uPx*.6), 3.)) - .5)*.03, sin(p.y*uPx*1.1)*.02, 1.));   // brushed: fine horizontal lines
    float e = envM(reflect(-v, n), t);
    if (edge) col = base*e*1.35 + v3(e*e*.05);
    else {
      float la = t*.25 + .7 + uTilt.x*2.;
      float dd = dot(p, float2(cos(la), sin(la))) - .35*sin(t*.3) - uTilt.y;
      float sh = exp(-dd*dd*4.);
      col = base*(e*.8 + sh*.38 + .05) + v3(e*e*.025 + sh*.03);
    }
  }
  if (!edge){
    // etched text on the plate (moved up so it clears the carved border): frosted, only on big badges
    float m = detA(p, .09)*smoothstep(80., 180., uPx);
    if (m > 0.){
      float grain = .85 + .3*hash(floor(gFrag));
      float3 fr = float3(.62, .63, .67)*(.85 + .12*min(envM(float3(.3, .4, 1.), t), 1.2))*grain;
      col = lerp(col, fr, m*.92);
    }
  }
  // no grey pocket round the stone: only the diamond / ruby, set straight on the plate with a soft shadow
  float2 pe = p - SC; float2 q = abs(pe);
  float gg = max(max(q.x/SH.x, q.y/SH.y), (q.x + q.y)/(SH.x + SH.y - SCLIP));
  col *= 1. - .55*smoothstep(1.16, .98, gg - pe.y*.15);
  float4 top = float4(0., 0., 0., 0.);
  if (gg < 1.02){
    float an = atan2(pe.y/SH.y, pe.x/SH.x);
    top = brill(gg*float2(cos(an), sin(an)), gg, aa/SH.y*1.3, t);
  }
  col = 1. - exp(-max(col, float3(0., 0., 0.))*1.6);
  col = col*(1. - top.a) + top.rgb;
  float4 pl = float4(col*inside, inside);
  return pv + pl*(1. - pv.a);
}

float4 main(float4 sv : SV_Position) : SV_Target {
  gFrag = float2(sv.x - uOff.x, uRes.y - (sv.y - uOff.y));
  float S = min(uRes.x, uRes.y);
  float2 p = (gFrag - .5*uRes)/(.5*S)*1.04;
  float aa = 2./S*1.04;
  float t = uTime + uSeed;
  if (uKind > 1.5) return caseM(p, aa, t);
  return caseS(p, aa, t);
}
"#;

/// Source of the bubble pixel shader (entry `main`).
pub fn bubble_src() -> String {
    let mut s = String::with_capacity(COMMON.len() + BUBBLE.len());
    s.push_str(COMMON);
    s.push_str(BUBBLE);
    s
}

/// Source of the metal / stone pixel shader (entry `main`).
pub fn gem_src() -> String {
    let mut s = String::with_capacity(COMMON.len() + GEM.len());
    s.push_str(COMMON);
    s.push_str(GEM);
    s
}
