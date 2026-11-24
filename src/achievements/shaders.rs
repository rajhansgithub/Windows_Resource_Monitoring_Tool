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
  float2 uTilt; float uIconOn; float uP4;
  float3 uWear; float uLayer;             // age: dust, seed, cobweb (0..1); uLayer: 0 colour, 1 age layer L, 2 age haze h
};
SamplerState smp : register(s0);
Texture2D<float> uIcon : register(t0);     // 512x512 alpha (white-on-clear icon), mip chain
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
#define uEdgeW 0.61
#define uEdgeVary 1.0
#define uEdgeCol 0.61
#define uSat 1.1
#define uFilmIn 1.0
#define uRefl 0.51
#define uFlow 2.0
#define uThick 540.0
#define uRim 1.0
#define uDotRefr 0.1
#define uDotRing 0.75
#define uRefrT 20.0
#define uGlow 0.0
#define uBreath 4.0
#define uMotAmt 1.0
#define uMotT 30.0
#define uVar 2.0
// the liquid pulse (water + tiny bubbles), tuner defaults
#define uLqRefr 0.0
#define uLqRefl 0.0
#define uLqFilm 0.5
#define uLqTint 1.0
#define uLqWob 0.56
#define uLqDrops 0.0
#define uLqDropS 0.4
#define uLqMove 0.2

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
// Resource Monitor mark: the pulse line only, centred (24-unit box); breathes very slightly.
float4 logo(float2 q, float aa){
  float cs = 1. + uMotAmt*.05*sin(uTime*TAU/uMotT);
  q /= cs;
  float2 p = float2(12. + q.x*9., 9.4 - q.y*9.);
  float a = aa*9.;
  float pl = min(min(min(seg(p, float2(4., 9.8), float2(7.6, 9.8)), seg(p, float2(7.6, 9.8), float2(9.6, 6.1))), min(seg(p, float2(9.6, 6.1), float2(11.4, 13.4)), seg(p, float2(11.4, 13.4), float2(13.2, 4.8)))), min(seg(p, float2(13.2, 4.8), float2(14.9, 9.8)), seg(p, float2(14.9, 9.8), float2(20., 9.8)))) - .95;
  float m = smoothstep(a, -a, pl);
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
// the pulse as a tube of liquid seen from the front (as in the preview's 3D bubble): distance to its centre line, icon units
float pulseD(float2 p){
  return min(min(min(seg(p, float2(4., 9.8), float2(7.6, 9.8)), seg(p, float2(7.6, 9.8), float2(9.6, 6.1))), min(seg(p, float2(9.6, 6.1), float2(11.4, 13.4)), seg(p, float2(11.4, 13.4), float2(13.2, 4.8)))), min(seg(p, float2(13.2, 4.8), float2(14.9, 9.8)), seg(p, float2(14.9, 9.8), float2(20., 9.8))));
}
// shading of clear water at normal n (view straight on), over what is behind it (the glow, sampled bent by refraction)
float3 water(float3 n, float2 q, float3 dc, float k){
  float ndv = n.z;
  float Fl = .02 + .98*pow(1. - ndv, 5.);
  float3 Rr = reflect(float3(0., 0., -1.), n);
  float3 behind = glow(q - n.xy*.22*uLqRefr, k)*.9 + v3(.012);
  float edge = lerp(1., smoothstep(.05, .55, ndv), saturate(uLqRefr*1.6));
  float3 trans = behind*(1. - Fl)*edge;
  float spec = (Fl*(.4 + uRefl)*1.4 + pow(max(dot(Rr, normalize(float3(-.45, .55, .7))), 0.), 120.)*2.4 + pow(max(dot(Rr, normalize(float3(.6, .35, .72))), 0.), 400.)*2.)*uLqRefl*2.;
  float3 fl = film(uThick*.5 + 220.*n.y + 60.*sin(q.x*5.6 + uTime*.4), ndv)*(Fl*2.2 + .03)*uLqFilm*1.6;
  float3 tint = dc*(.08 + .5*pow(ndv, 2.))*uLqTint*(.85 + .25*k);
  return trans + v3(spec) + fl + tint;
}
// rgb = the water pulse with its tiny bubbles (already over the glow behind it), a = coverage. dc = the logo colour here.
float4 liquid(float2 q, float aa, float3 dc, float k){
  float t = uTime;
  float2 w = q*.62;                                   // bubble units, as in the preview's 3D pulse
  w.y -= .012*uLqMove*sin(t*.6);
  w = rotv(.035*uLqMove*sin(t*.37), w);
  float cs = 1. + uMotAmt*.05*sin(t*TAU/uMotT);
  w /= cs;
  float2 p = float2(12. + w.x/.62*9., 9.4 - w.y/.62*9.);
  float R = .95*(1. + (.006*sin(w.x*14. - t*2.1) + .003*sin(w.x*33. + w.y*9. + t*3.3))*uLqWob*2./.062);
  float d0 = pulseD(p);
  float a = aa*9.;
  float m = smoothstep(a, -a, d0 - R);
  float3 n = float3(0., 0., 1.);
  if (m > 0.){
    float e = .06;
    float2 gr = float2(pulseD(p + float2(e, 0.)) - pulseD(p - float2(e, 0.)), pulseD(p - float2(0., e)) - pulseD(p + float2(0., e)))/(2.*e);
    float x = clamp(d0/R, 0., .999);
    n = normalize(float3(gr*x, sqrt(1. - x*x)));
  }
  // tiny bubbles (spheres): the one in front wins
  [loop] for (int i = 0; i < 12; i++){
    float fi = (float)i;
    if (fi >= 12.*uLqDrops) break;
    float2 c = float2(-.6 + fi*.105 + .03*sin(fi*7.3), .30*sin(fi*2.39 + .5) - .03);
    c += .014*uLqMove*float2(sin(t*.5 + fi), sin(t*.43 + fi*2.));
    float r = (.013 + .026*frac(fi*.618))*uLqDropS;
    float2 v = (w - c)/r; float l2 = dot(v, v);
    float md = smoothstep(1. + aa*.62/r, 1. - aa*.62/r, sqrt(l2));
    if (md > 0.){
      float3 nd = float3(v, sqrt(max(1. - l2, 0.)));
      if (md >= m || nd.z > n.z){ n = nd; }
      m = max(m, md);
    }
  }
  if (m <= 0.) return float4(0., 0., 0., 0.);
  return float4(water(n, q, dc, k), m);
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
  if (uVar > 1.5 && uVar < 2.5){
    float4 lq = liquid(q, aa, dotc, k);
    return g*(1. - lq.a) + lq.rgb*lq.a;
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

/// Metals, Diamond and Ruby (gem.js) as Uptime Medals: reference shape "round" (shape 0). Metals = `medal` (struck
/// medal: polished raised rim, spun face with fine rings, icon engraved with a frosted floor and bright cut walls);
/// Diamond / Ruby = `brill` (round brilliant, ik = -1: no etching on the stone). `envM`, `env`, `inner` as in gem.js.
/// Metals light from `envS` (a photo studio: softbox, strip light, dark room, horizon) with ACES tone mapping, as in the
/// preview page (2026-11-22). The viewer tilt (`uTilt`, 0 by default) turns that studio.
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

// the round brilliant, drawn in p (|p| = 1 at the girdle)
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

// icon mask: 512 px square, half size .66 (badge units) centred on the medal; mip chosen from the on-screen size
float icA(float2 p){
  float2 iu = p/.66*.5 + .5;
  if (iu.x < 0. || iu.y < 0. || iu.x > 1. || iu.y > 1.) return 0.;
  float lod = max(log2(512.*1.04/(.66*max(uPx, 8.))), 0.);
  return uIcon.SampleLevel(smp, float2(iu.x, 1. - iu.y), lod);
}
// ---- struck metal medals (tiers 1-4): polished raised rim, spun face with fine rings, icon engraved (frosted floor, bright cut walls)
// studio for the metals: a dark room with a big softbox up left (close to the camera, so the face catches part of it),
// a tall strip light right, a warm floor bounce, a thin horizon line and one hard glint. Moves with the full view's tilt.
float envS(float3 d, float t){
  d.xy = rotv(.12*sin(t*.21) + uTilt.x*.8, d.xy);
  d.yz = rotv(uTilt.y*.6 + .04*sin(t*.17), d.yz);
  float s = .09 + .12*smoothstep(-.2, .9, d.y);
  float3 a = normalize(float3(-.42, .46, .78));
  float3 ux = normalize(cross(float3(0., 0., 1.), a)), uy = cross(a, ux);
  float ca = dot(d, a);
  if (ca > 0.){ float2 q = float2(dot(d, ux), dot(d, uy))/ca; s += 2.6*smoothstep(.08, -.08, max(abs(q.x) - .55, abs(q.y) - .40))*(.8 + .2*smoothstep(-.3, .3, q.y)); }
  float3 b = normalize(float3(.82, 0., .57)); float cb = dot(d, b);
  if (cb > 0.){ float3 bx = normalize(cross(float3(0., 1., 0.), b)); float2 q = float2(dot(d, bx), d.y)/cb; s += 1.8*smoothstep(.05, -.05, max(abs(q.x) - .08, abs(q.y) - 1.)); }
  s += .32*smoothstep(-.05, -.7, d.y)*smoothstep(-.4, .2, d.z);
  s += .25*exp(-pow((d.y + .1)/.03, 2.))*smoothstep(-.5, .2, d.z);
  s += 9.*pow(max(dot(d, normalize(float3(.3, .62, .72))), 0.), 700.);
  return s;
}
float3 aces(float3 x){ return saturate((x*(2.51*x + .03))/(x*(2.43*x + .59) + .14)); }
float4 medal(float2 p, float r, float aa, float t){
  float3 base = uMetal;
  float3 v = normalize(float3(-p*.30, 1.));                // a near camera
  float2 rd = r > 1e-4 ? p/r : float2(1., 0.);
  float RIM = .83;
  float3 col;
  if (r > RIM){
    // polished raised rim: a rounded bead, then a crisp outer edge. Mirror finish: one sharp reflection.
    float x = clamp((r - RIM)/(.975 - RIM), 0., 1.);
    float sl = cos(x*PI)*1.25;
    sl = lerp(sl, -2.6, smoothstep(.972, .99, r));
    float3 n = normalize(float3(-rd*sl, 1.));
    float3 R = reflect(-v, n);
    float cv = saturate(dot(n, v));
    float3 F = base + (1. - base)*pow(1. - cv, 5.);
    col = F*envS(R, t);
    col *= 1. - .45*smoothstep(.16, 0., x);              // the face's shadow where the bead rises
  } else {
    // spun face, very slightly domed: the rings smear each reflection along the radius (the bow-tie of real spun metal)
    float3 n = normalize(float3(p*.22, 1.));
    float ring = sin(r*uPx*.9)*.5 + .5;
    n = normalize(n + float3(rd*(ring - .5)*.012, 0.));
    float e = 0.;
    [unroll] for (int k = 0; k < 4; k++){ float o = (float(k) - 1.5)/1.5; e += envS(reflect(-v, normalize(n + float3(rd*o*.28, 0.))), t); }
    e *= .25;
    float cv = saturate(dot(n, v));
    float3 F = base + (1. - base)*pow(1. - cv, 5.);
    col = F*e*.92;
    col *= 1. - .35*smoothstep(RIM - .12, RIM, r);      // occlusion under the rim
  }
  // a fine groove where the rim meets the face
  col *= 1. - smoothstep(aa*1.5, 0., abs(r - RIM))*.7;
  // engraved icon: sunken, sandblasted floor (matte, darker) and polished walls that catch the studio
  if (uIconOn > 0. && r < RIM){
    float d = 1.6/max(uPx, 40.);
    float m = icA(p);
    float gx = icA(p + float2(d, 0.)) - icA(p - float2(d, 0.));
    float gy = icA(p + float2(0., d)) - icA(p - float2(0., d));
    if (m + abs(gx) + abs(gy) > 0.){
      float grain = .85 + .3*hash(floor(gFrag));
      float diff = .10 + .10*envS(float3(-.35, .4, .85), t);
      float3 floorC = base*diff*grain;
      col = lerp(col, floorC, smoothstep(.15, .85, m));
      float3 nw = normalize(float3(float2(gx, gy)*2.2, 1.));
      float w = saturate(length(float2(gx, gy))*1.6);
      col = lerp(col, base*envS(reflect(-v, nw), t)*1.1, w*.9);
    }
  }
  float inside = smoothstep(1. + aa, 1. - aa, r);
  col = aces(col*1.1);
  return float4(col*inside, inside);
}

// ---- age: dust and cobwebs (uWear.x dust, .y this medal's seed, .z cobweb age). 1 real hour = 1 day on the shelf.
// Same as the preview page (docs/preview/js/gem.js and js/age.js), line for line. Look = the owner's saved tuning (366 days, 2026-11-22).
// Dust is grains, not a film: a few grains first, more as it builds, until they join into a grainy grey layer (a year).
// It lies thick on flat surfaces and piles up in hollows; steep walls stay almost clean; the top gets it before the bottom.
// Cobweb: it grows, it never scales. Every thread has a fixed place and a birth time (hash of this medal's seed); time only
// reveals them: hair-thin threads from the rim, more between them, beaded drop lines, then torn gauze. Dust turns them into
// grey furry ropes that sag. Every medal has its own web (where it hangs, which threads) and its own dust pattern.
static const float4 AGE0 = float4(2., 1., 1., 1.);      // dust amount (used by the clock in core.rs), patchiness, top first, hides reflections
static const float4 AGE1 = float4(1.5, 1.29, 1.3, 1.4); // grit, lint, web amount, how many threads
static const float4 AGE2 = float4(1.42, .65, .3, 1.6);  // thread thickness, glint, gauze, dust on silk
static const float  AGE_RAINBOW = .19;
float wN(float2 p){ return vnoise(p); }
static float wS, wC;   // this medal's seed (uWear.y, from its id) and the angle its cobweb hangs at: every medal gets its own web and dust
float wF(float2 p){ return .5*wN(p)+.25*wN(p*2.03+3.1)+.125*wN(p*4.01+7.7); }
float2 wSeg(float2 p, float2 a, float2 b){ float2 pa=p-a, ba=b-a; float h=clamp(dot(pa,ba)/dot(ba,ba),0.,1.); return float2(length(pa-ba*h),h); }
// short curled fibres, one per cell of size 1/sc. dens = how many of them exist yet (each has its own fixed place).
float wHair(float2 p, float sc, float w, float aa, float dens){
  float2 q=(p+(float2(wN(p*11.+2.),wN(p*11.+8.))-.5)*.07)*sc, id=floor(q);
  float m=0.;
  [loop] for(int j=-1;j<=1;j++) [loop] for(int i=-1;i<=1;i++){
    float2 c=id+float2(float(i),float(j));
    if(hash(c+13.7)<dens){
      float2 o=c+float2(hash(c+3.1),hash(c+5.3));
      float an=hash(c+7.9)*3.1416;
      float2 dir=float2(cos(an),sin(an))*(.25+.45*hash(c+11.));
      float d=wSeg(q,o-dir,o+dir).x/sc;
      m=max(m,(1.-smoothstep(max(w-aa,0.)*.4,w+aa,d))*(.5+.5*hash(c+17.)));
    }
  }
  return m*min(1.,w*2.4/(aa*1.5+1e-4));
}
// rope i of the web: where it is tied (a on the rim; b on the rim for even i, on the face for odd i) and how far it sags
void wRope(float i, float G, out float2 a, out float2 b, out float sg){
  float sd=wS*61.7, h1=hash(float2(i*1.31+.7+sd,2.9)), h2=hash(float2(i*2.17+4.1,7.3+sd)), h3=hash(float2(i*.73+9.2+sd,1.1));
  float a1=wC-1.175+2.35*frac(i*.381966+.13+wS*.37);
  a=float2(cos(a1),sin(a1))*.955;
  float a2=clamp(a1+(h1>.5?1.:-1.)*(.6+.8*h2),wC-1.425,wC+1.425);
  float2 f=float2(-.55+.7*h1,.6-.7*h2); float r0=wC-2.175, cs=cos(r0), sn=sin(r0);
  b=fmod(i,2.)<.5 ? float2(cos(a2),sin(a2))*.955 : float2(f.x*cs-f.y*sn,f.x*sn+f.y*cs);
  sg=(.05+.11*h3)*length(b-a)*(.5+.9*clamp((G-i*.055)*1.2,0.,1.));   // dust loads it: it sags more with age
}
// the point at t along rope i
float2 wOn(float i, float t, float G){
  float2 a,b; float sg; wRope(i,G,a,b,sg);
  float2 cu=normalize(b-a), nn=float2(-cu.y,cu.x); if(dot(nn,float2(.35,-1.))<0.) nn=-nn;
  return lerp(a,b,t)+nn*sg*4.*t*(1.-t);
}
// one thin sagging thread from a to b: uneven width, beads of dust, a soft fuzz. grow: 0..1+ how much of it is spun yet.
float wLine(float2 p, float2 a, float2 b, float sg, float w, float aa, float seed, float grow, float fuzz){
  float2 ch=b-a; float L=length(ch); float2 cu=ch/L, nn=float2(-cu.y,cu.x); if(dot(nn,float2(.35,-1.))<0.) nn=-nn;
  float u=dot(p-a,cu)/L, h=dot(p-a,nn);
  float uc=clamp(u,0.,1.), s=sg*4.*uc*(1.-uc), ds=sg*4.*(1.-2.*uc)/L;
  float d=abs(h-s)/sqrt(1.+ds*ds);
  if(u<-.03||u>1.03||d>w*9.+aa*3.) return 0.;
  float ww=w*(.55+.9*wN(float2(uc*L*26.,seed)))*(1.+1.6*smoothstep(.78,.95,wN(float2(uc*L*64.,seed+3.))));
  float core=(1.-smoothstep(max(ww-aa,0.)*.4,ww+aa,d))*min(1.,ww*2.4/(aa*1.5+1e-4));
  float halo=(1.-smoothstep(0.,ww*4.5+aa*2.,d))*.22*fuzz*(.6+.8*wN(p*110.+seed));
  return max(core,halo)*smoothstep(-.02,.01,u)*(1.-smoothstep(.99,1.02,u))*(1.-smoothstep(grow-.08,grow,uc));
}
// the cobweb: rgb = its light (premultiplied, never above a), a = how much it covers
float4 wWeb(float2 p, float amt, float dust, float aa){
  if(amt<=.001 || dot(p,float2(cos(wC),sin(wC)))<-.5) return float4(0., 0., 0., 0.);   // it lives on this medal's upper side (left, top or right)
  float G=min(amt,1.3), keep=.75*AGE1.w, W=AGE2.x;
  float rope=0., fine=0., gz=0., shd=0.;
  float thrH=.24+.18*smoothstep(.55,1.,G);                      // old gauze tears: the holes only grow
  [loop] for(int i=0;i<8;i++){
    float fi=float(i), age=G-fi*.055;
    if(age>0. && (i<2 || hash(float2(fi+wS*53.,3.3))<keep)){
      float2 a,b; float sg; wRope(fi,G,a,b,sg);
      float2 ch=b-a; float L=length(ch); float2 cu=ch/L, nn=float2(-cu.y,cu.x); if(dot(nn,float2(.35,-1.))<0.) nn=-nn;
      float u=dot(p-a,cu)/L, h=dot(p-a,nn);
      if(u>-.03 && u<1.03){
        float uc=clamp(u,0.,1.), s=sg*4.*uc*(1.-uc), ds=sg*4.*(1.-2.*uc)/L;
        float e=h-s, d=abs(e)/sqrt(1.+ds*ds);
        float w=(i<2?.016:.006+.008*hash(float2(fi+wS*53.,8.8)))*W*(.12+.88*smoothstep(0.,.6,age))*(.6+.6*dust*AGE2.w);
        shd=max(shd,(1.-smoothstep(0.,w*5.+.01,abs(e-.014-w)))*smoothstep(-.02,.03,u)*(1.-smoothstep(.97,1.02,u))*min(age*8.,1.));   // its soft shadow on the medal
        if(d<w*9.+aa*3.){
          float ww=w*(.55+.9*wN(float2(uc*L*26.,fi*7.+1.)))*(1.+1.6*smoothstep(.78,.95,wN(float2(uc*L*64.,fi*5.+3.))));
          float dr=d+(wN(p*75.+fi*3.)-.5)*ww*1.3;                // a ragged, furry outline
          float core=(1.-smoothstep(ww*.25,ww*1.15+aa,dr))*min(1.,ww*2.4/(aa*1.5+1e-4))*(.62+.3*wN(float2(uc*L*40.,e*160.)+fi));
          float halo=(1.-smoothstep(ww*.5,ww*3.4+aa*2.,dr))*.42*smoothstep(0.,.4,age)*(.35+1.1*wN(p*130.+fi));   // fur
          rope=max(rope,max(core,halo)*smoothstep(-.02,.01,u)*(1.-smoothstep(.99,1.02,u))*(1.-smoothstep(age/.07-.08,age/.07,uc)));
        }
        float gage=age-.2-.2*hash(float2(fi+wS*53.,6.1));               // gauze: later, on the rim-to-rim ropes
        if(gage>0. && u>0. && u<1.){
          float skirt=(.03+.16*wF(float2(uc*L*6.,fi*3.+2.)))*min(gage*3.,1.);   // a ragged hem hangs below the rope
          float reg=e<0. ? (fmod(fi,2.)<.5 ? smoothstep(0.,.05,.955-length(p)) : 0.) : 1.-smoothstep(skirt*.3,skirt,e);
          if(reg>0.){
            float holes=smoothstep(thrH-.12,thrH+.1,wF(p*5.5+fi*3.7+wS*9.));
            float fibre=(.55+.45*wN(float2(uc*L*9.+fi,e*85.)))*(.7+.3*wN(p*70.));
            gz=max(gz,reg*holes*(.4+.6*fibre)*smoothstep(0.,.35,gage)*smoothstep(0.,.05,uc)*(1.-smoothstep(.95,1.,uc)));
          }
        }
      }
    }
  }
  [loop] for(int k=0;k<12;k++){                                       // fine threads from rope to rope
    float fk=float(k);
    float ia=floor(hash(float2(fk+wS*53.,1.2))*7.99), ib=fmod(ia+1.+floor(hash(float2(fk+wS*53.,2.6))*2.99),8.);
    float age=G-max(.08+.065*fk,max(ia,ib)*.055+.03);
    if(age>0. && hash(float2(fk+wS*53.,4.4))<keep && (ia<1.5||hash(float2(ia+wS*53.,3.3))<keep) && (ib<1.5||hash(float2(ib+wS*53.,3.3))<keep)){
      float2 a=wOn(ia,.15+.7*hash(float2(fk+wS*53.,5.5)),G), b=wOn(ib,.15+.7*hash(float2(fk+wS*53.,6.6)),G);
      float m=wLine(p,a,b,.03*length(b-a),.0017*W*(.45+.55*smoothstep(0.,.5,age)),aa,fk*3.1,age/.06,smoothstep(0.,.5,age));
      if(G>.8 && hash(float2(fk+wS*53.,9.9))>.7){ float u=wSeg(p,a,b).y; m*=smoothstep(.0,.1,abs(u-.5)-.1*smoothstep(.8,1.,G)); }   // an old one snaps
      fine=max(fine,m);
    }
  }
  [loop] for(int k=0;k<6;k++){                                        // drop lines hanging from the ropes, beaded with dust
    float fk=float(k), ia=floor(hash(float2(fk+wS*53.,11.2))*7.99);
    float age=G-max(.05+.08*fk,ia*.055+.02);
    if(age>0. && hash(float2(fk+wS*53.,14.4))<keep && (ia<1.5||hash(float2(ia+wS*53.,3.3))<keep)){
      float2 a=wOn(ia,.2+.6*hash(float2(fk+wS*53.,15.5)),G);
      float2 b=a+float2((hash(float2(fk+wS*53.,16.6))-.5)*.1,-(.09+.2*hash(float2(fk+wS*53.,17.7))));
      fine=max(fine,wLine(p,a,b,.004,.0016*W*(.45+.55*smoothstep(0.,.5,age)),aa,fk*5.3+40.,age/.05,smoothstep(0.,.5,age)));
    }
  }
  float gza=clamp(gz*AGE2.z*2.3,0.,.8);
  float al=clamp(max(rope,fine*.5)+gza*(1.-rope),0.,.94);
  float3 silk=float3(.86,.85,.83), dusty=float3(.70,.69,.66);
  float lite=.86+.14*smoothstep(-1.,1.,p.y)+.12*AGE2.y*fine*(1.-dust*.6)-.14*gza*(1.-rope);   // fresh silk glints; dusty silk does not
  float3 rgb=lerp(silk,dusty,clamp(AGE2.w*(.45+.55*dust),0.,1.))*al*lite
          +AGE_RAINBOW*max(rope,fine)*(1.-dust)*.3*(.5+.5*cos(6.283*(p.x*5.+p.y*3.+float3(0.,.33,.67))));
  return float4(min(rgb,v3(al)),al+(1.-al)*max(shd*.4,gza*.15));
}
float4 wOver(float4 L, float3 c, float k){ return float4(L.rgb*(1.-k)+c*k,L.a+k-L.a*k); }   // a layer over a premultiplied layer
// The age of one spot, as layers over its colour that do not depend on the colour (so a still medal can work them out
// once and keep them): h = the haze (amount, tint, grey), L = dust, fibres and cobweb as one premultiplied layer.
// p: on the face (radius 1, +y up). recess: how sheltered (dust piles up there). flt: 1 on a flat surface, lower on a
// steep wall (dust slides off). False: nothing on it.
bool ageLayers(float2 p, float recess, float flt, float aa, out float3 h, out float4 L){
  float dust=min(uWear.x,1.), web=uWear.z*AGE1.z;
  wS=uWear.y; wC=2.175+(wS-.5)*2.;   // upper left on average; from the top right to the left side
  float2 ps=p+wS*float2(3.7,5.3);         // this medal's own dust pattern
  h=v3(0.); L=float4(0., 0., 0., 0.);
  if(dust+web<.002) return false;
  float rc=min(recess,1.);
  // how much has settled here: the top first (the bottom's share = dust^(1+2*how low)), little on steep walls, more in hollows
  float low=1.-smoothstep(-1.,1.,p.y+(wF(p*2.5+3.)-.5)*.3);
  float dl=pow(max(dust,1e-4),1.+2.*low*AGE0.z);
  float wall=1.-smoothstep(.72,.97,flt);
  float T=dl*lerp(1.,lerp(.05,.7,dl*dl),wall)*(1.+1.3*rc);
  float Tc=min(T,1.);
  // grains: clumps, coarse grains, fine grains. A grain is there once the layer passes its own threshold, so more dust =
  // more grains (none ever moves). Small tiles cannot show the fine ones (f1, f2 fade them out: no shimmer).
  float f1=clamp(2.-aa*38.,0.,1.), f2=clamp(2.-aa*95.,0.,1.), f3=clamp(1.6-aa*170.,0.,1.);
  float clump=wF(ps*4.3+11.)/.875;
  float g1=wN(ps*38.+1.), g2=wN(ps*95.+7.), g3=wN(ps*170.+4.);
  float n=lerp(.5,clump,AGE0.y)*.42+lerp(.5,g1,f1)*.2+lerp(.5,g2,f2)*.24+lerp(.5,g3,f3)*.14;
  float thr=.80-.72*Tc;
  float cov=smoothstep(thr-.03,thr+.05,n)*smoothstep(0.,.04,T);
  float thick=smoothstep(0.,.3,n-thr);
  float2 Ld=float2(-.6,.8)*.006;                                  // towards the light
  float sh=clamp(1.+((g1-wN((ps+Ld)*38.+1.))*.35*f1+(g2-wN((ps+Ld*.5)*95.+7.))*.65*f2)*1.1,.78,1.22);
  // a thin layer: the mirror goes hazy (dust scatters the light), the colour still shows through (see ageApply)
  h=float3(smoothstep(0.,.6,T)*AGE0.w,(.17+.07*smoothstep(-1.,1.,p.y))*(1.-.12*rc),1.-.12*rc);
  // the grains themselves: light grey, each a little different, lit from the top left; dark specks and pale grains
  float3 dc=float3(.60,.585,.55);
  float tone=(.50+.10*smoothstep(-1.,1.,p.y)+.16*thick)*(.80+.22*lerp(.5,g2,f2)+.22*lerp(.5,g3,f3)+.25*(clump-.5)+.3*(wF(ps*13.+2.)-.44))*sh*(1.-.18*rc);
  float3 dustc=dc*tone;
  dustc*=1.-.5*smoothstep(.86,.95,wN(ps*150.+3.))*f2*AGE1.x;
  dustc+=dc*.22*smoothstep(.9,.97,wN(ps*60.+9.))*f1*AGE1.x;
  L=wOver(L,dustc,cov*(.45+.52*thick));
  // lint: long fibres lie anywhere (on clean metal too), short fuzz grows in the thick dust
  float lint=wHair(ps,8.,.0022,aa,.4*smoothstep(.05,.7,dl)*AGE1.y);
  float fuzz=wHair(ps+5.3,19.,.0017,aa,.8*smoothstep(.3,.9,T)*AGE1.y);
  L=wOver(L,float3(.72,.71,.68),lint*.5);
  L=wOver(L,float3(.70,.69,.66),fuzz*.4);
  float4 wb=wWeb(p,web,dust,aa);
  L=float4(L.rgb*(1.-wb.a)+wb.rgb,L.a+wb.a-L.a*wb.a);
  return true;
}
// put the age layers on a premultiplied colour (coverage a)
float3 ageApply(float3 col, float a, float3 h, float4 L){
  float mx=max(max(col.r,col.g),max(col.b,.001));
  col=lerp(col,(col/mx*h.y+float3(.15,.145,.135)*h.z)*a,h.x);
  return col*(1.-L.a)+L.rgb*a;
}
// age a (premultiplied) colour (see ageLayers). metal: 1 metal, 0 stone.
float3 aged(float3 col, float a, float2 p, float recess, float flt, float metal, float aa){
  float3 h; float4 L;
  if(a<=0. || !ageLayers(p,recess,flt,aa,h,L)) return col;
  return ageApply(col,a,h,L);
}

float4 main(float4 sv : SV_Position) : SV_Target {
  gFrag = float2(sv.x - uOff.x, uRes.y - (sv.y - uOff.y));
  float S = min(uRes.x, uRes.y);
  float2 p = (gFrag - .5*uRes)/(.5*S)*1.04;
  float r = length(p);
  float aa = 2./S*1.04;
  float t = uTime + uSeed;
  if (r > 1. + aa) return float4(0., 0., 0., 0.);
  float rec = uKind > 1.5 ? (uIconOn > 0. && r < .83 ? icA(p) : 0.) + smoothstep(.1, 0., abs(r - .83))*.8 + smoothstep(.62, .83, r)*step(r, .83)*.3
                          : smoothstep(.86, 1., r)*.6;
  float fl = 1. - .3*smoothstep(.5, 1., r);            // how flat it is here: dust slides off the steep parts
  if (uKind > 1.5){
    fl = 1.;
    if (r > .83){ float sl = cos(clamp((r - .83)/(.975 - .83), 0., 1.)*PI)*1.25; fl = 1./sqrt(1. + sl*sl); }   // the flanks of the rim bead
    else if (uIconOn > 0. && uWear.x > 0.){                                                                  // the walls of the engraving
      float e = max(aa, .008);
      float2 gi = float2(icA(p + float2(e, 0.)) - icA(p - float2(e, 0.)), icA(p + float2(0., e)) - icA(p - float2(0., e)));
      fl = 1./sqrt(1. + dot(gi, gi)*5.);
    }
  }
  // The full view asks for the age layers once per medal (they do not change while it moves) and puts them on each
  // frame itself (`ui.rs`, age_apply): the dust and cobweb were most of a frame's cost.
  if (uLayer > .5){
    float3 h; float4 L;
    if (!ageLayers(p, rec, fl, aa, h, L)) return float4(0., 0., 0., 0.);
    return uLayer < 1.5 ? L : float4(h.x, h.y*4., h.z, 1.);
  }
  float4 c = uKind > 1.5 ? medal(p, r, aa, t) : brill(p, r, aa, t);
  c.rgb = aged(c.rgb, c.a, p, rec, fl, uKind > 1.5 ? 1. : 0., aa);
  return c;
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

/// The Dial family coin (preview v7, `docs/preview/js/3d.js` COIN with uDesign = 6), ray marched in 3D. One program per
/// stone (`#define STONE`: 0 CPU die, 1 Diamond, 2 Ruby, 3 Emulsion's open ring) and two age-layer programs (`LAYER`).
/// Port notes as above, plus: GLSL `uR * v` (coin to world) = `toW`, `uRT * v` = `toC`; the studio's slow sway `ER` is
/// kept as (cos, sin) in `gER`. Not ported: the other coin designs (0-5).
/// Age: dust and cobweb do not move with the coin, so the heavy part (`ageLayers`: grains, lint, the web) is worked out
/// once per coin and wear step, face on, by the LAYER program into a texture (front | back), and the colour program
/// only adds the haze (cheap) and lays that texture on: the same look as the preview's per-pixel `aged`, at a fraction of
/// the cost of a frame. The Emulsion coin never ages.
pub const COIN: &str = r#"
cbuffer K : register(b1) {
  float4 kR0; float4 kR1; float4 kR2;   // rows of the coin-to-world rotation; .w: camera half-size, ring text on, back text on
  float4 kM2;                            // the second metal; .w: age texture on
};
Texture2D<float> uRing : register(t1);   // RING_N alpha: tier text round the front (paint-filled), spans the coin (k = 1)
Texture2D<float> uBack : register(t2);   // RING_N alpha: the name on the reverse (engraved), k = .92
Texture2D<float4> uAgeL : register(t3);  // age layer L (premultiplied), front in the left half, back in the right
float3 toW(float3 v){ return float3(dot(kR0.xyz, v), dot(kR1.xyz, v), dot(kR2.xyz, v)); }
float3 toC(float3 v){ return kR0.xyz*v.x + kR1.xyz*v.y + kR2.xyz*v.z; }

// the studio for metal (constant light vectors; the slow sway is set once per pixel)
static float2 gER = float2(1., 0.);
static const float3 EA=float3(-0.420758,0.460830,0.781408), EUX=float3(-0.738485,-0.674269,0.), EUY=float3(0.526879,-0.577058,0.624021);
static const float3 EB=float3(0.821109,0.,0.570771), EBX=float3(0.570771,0.,-0.821109), ESP=float3(0.301086,0.622244,0.722606);
float envS(float3 d, float t){
  d.xy=float2(gER.x*d.x + gER.y*d.y, -gER.y*d.x + gER.x*d.y);
  float s=.09+.12*smoothstep(-.2,.9,d.y);
  float ca=dot(d,EA);
  if(ca>0.){ float2 q=float2(dot(d,EUX),dot(d,EUY))/ca; s+=2.6*smoothstep(.08,-.08,max(abs(q.x)-.55,abs(q.y)-.40))*(.8+.2*smoothstep(-.3,.3,q.y)); }
  float cb=dot(d,EB);
  if(cb>0.){ float2 q=float2(dot(d,EBX),d.y)/cb; s+=1.8*smoothstep(.05,-.05,max(abs(q.x)-.08,abs(q.y)-1.)); }
  s+=.32*smoothstep(-.05,-.7,d.y)*smoothstep(-.4,.2,d.z);
  float gy=(d.y+.1)/.03; s+=.25*exp(-gy*gy)*smoothstep(-.5,.2,d.z);
  float sp=max(dot(d,ESP),0.); sp*=sp; sp*=sp; sp*=sp; sp*=sp;
  s+=9.*pow(sp,43.75);
  return s;
}
// the studio for the stone
float envG(float3 d){
  d.xy=rotv(uTime*.05, d.xy);
  float az=atan2(d.y,d.x);
  float s=.03+.45*smoothstep(-.1,.9,d.z);
  s+=1.1*pow(max(cos(az*3.)*.5+.5,0.),5.)*smoothstep(-.2,.5,d.z)*smoothstep(.98,.75,d.z);
  s+=pow(max(dot(d,float3(0.549478,0.549478,0.629402)),0.),140.)*8.;
  s+=pow(max(dot(d,float3(-0.723442,0.251195,0.643060)),0.),100.)*5.;
  s+=pow(max(dot(d,float3(0.050118,-0.781846,0.621469)),0.),180.)*7.;
  s+=1.5*smoothstep(.72,.9,d.z)*smoothstep(.99,.95,d.z)*(.75+.25*cos(az*8.));
  s*=1.-.9*smoothstep(.975,.995,d.z);
  return s;
}
float3 aces(float3 x){ return saturate((x*(2.51*x+.03))/(x*(2.43*x+.59)+.14)); }
// a mask spanning [-k, k] of the face, row 0 at the top; lod from the size on screen
float texM(Texture2D<float> t, float2 p, float k, float lod){
  float2 iu=p/k*.5+.5;
  if(iu.x<0. || iu.y<0. || iu.x>1. || iu.y>1.) return 0.;
  return t.SampleLevel(smp, float2(iu.x, 1.-iu.y), lod);
}

// the round brilliant (girdle radius 1, centred in z)
static const float TABC=0.602029;
static const float2 BEZ=float2(0.566406,0.824126); static const float BEZC=0.813336;
static const float2 STA=float2(0.358368,0.933580); static const float STAC=0.779264;
static const float2 UGI=float2(0.669131,0.743145); static const float UGIC=0.891796;
static const float2 PAV=float2(0.652760,-0.757565); static const float PAVC=0.456076;
static const float2 LGI=float2(0.669131,-0.743145); static const float LGIC=0.476191;
static const float2 ROT8=float2(0.923880,0.382683); static const float2 HALF8=float2(0.980785,0.195090);
// the stone sits forward in a raised collet: girdle at ZG6, the pavilion cut flat just inside the back plate (no bulge)
static const float SSD=.55;
static const float ZG6=.25;
static const float GZD=ZG6-.2795*SSD;
static const float FLOOR6=-.13;
static const float ZF=0., RF=.685;         // Emulsion: the bubble's sphere in the open ring
static const float SCUT=-(FLOOR6-GZD)/SSD;
#define P(N,C) { float3 nn=N; float dn=dot(nn,d); float tt=(C-dot(nn,o))/dn; if(dn<0.){ if(tt>tIn){ tIn=tt; nIn=nn; } } else { if(tt<tOut){ tOut=tt; nOut=nn; } } }
void trav(float3 o, float3 d, out float tIn, out float3 nIn, out float tOut, out float3 nOut){
  tIn=-1e9; tOut=1e9; nIn=float3(0.,0.,1.); nOut=float3(0.,0.,1.);
  P(float3(0.,0.,1.),TABC)
  P(float3(0.,0.,-1.),SCUT)
  float2 cs=float2(1.,0.);
  [unroll] for(int k=0;k<16;k++){
    float2 ch=float2(cs.x*HALF8.x-cs.y*HALF8.y, cs.y*HALF8.x+cs.x*HALF8.y);
    P(float3(cs,0.),1.)
    P(float3(ch*UGI.x,UGI.y),UGIC)
    P(float3(ch*LGI.x,LGI.y),LGIC)
    if((k&1)==0){ P(float3(cs*BEZ.x,BEZ.y),BEZC) P(float3(cs*PAV.x,PAV.y),PAVC) }
    else { P(float3(cs*STA.x,STA.y),STAC) }
    cs=float2(cs.x*ROT8.x-cs.y*ROT8.y, cs.y*ROT8.x+cs.x*ROT8.y);
  }
}
float fres(float c, float ior){ float F0=(ior-1.)/(ior+1.); F0*=F0; return F0+(1.-F0)*pow(1.-saturate(c),5.); }

// ---- shapes
float box2(float2 p, float2 h, float rr){ float2 d=abs(p)-h+rr; return length(max(d,0.))+min(max(d.x,d.y),0.)-rr; }
float box3(float3 p, float3 h, float rr){ float3 q=abs(p)-h+rr; return length(max(q,0.))+min(max(q.x,max(q.y,q.z)),0.)-rr; }
float ringS(float3 p, float R0, float R1, float z0, float z1, float rr){ return box2(float2(length(p.xy),p.z)-float2((R0+R1)*.5,(z0+z1)*.5), float2((R1-R0)*.5,(z1-z0)*.5), rr); }
float disc(float3 p, float R, float z0, float z1, float rr){ return box2(float2(length(p.xy),p.z-(z0+z1)*.5), float2(R,(z1-z0)*.5), rr); }
float2 prep(float2 q, float n){ float an=TAU/n; float a=modg(atan2(q.y,q.x)+an*.5,an)-an*.5; return length(q)*float2(cos(a),sin(a)); }
float2 U(float2 a, float2 b){ return b.x<a.x?b:a; }
// ids: 1 polished, 2 spun (turned), 3 black anodised, 4 sandblasted, 6 second metal polished, 7 silicon die
float2 mapF(float3 p){
  float r=length(p.xy);
  float2 d=float2(ringS(p,.9,1.,-.01,.19,.025),1.);
  if(r>.9){ float2 bq=prep(p.xy,72.); d=U(d,float2(length(float3(bq.x-.948,bq.y,p.z-.186))-.016,1.)); }
  else d.x=min(d.x,length(float2(.932-r,max(abs(p.z-.186)-.016,0.))));
  d=U(d,float2(max(ringS(p,.70,.905,-.01,.4,0.),(p.z-(.15+(r-.70)*.13))*.99),2.));
#if STONE == 3
  return d;                                          // Emulsion: open ring, nothing behind the bubble
#else
  d=U(d,float2(ringS(p,.595,.705,-.01,.08,.012),3.));
  if(r>.63) d.x=min(d.x,r-.6);
  else {
#if STONE == 0
    d=U(d,float2(disc(p,.6,-.01,.05,.005),4.));
    d=U(d,float2(box3(p-float3(0.,0.,.06),float3(.235,.235,.07),.022),7.));
    float2 a2=abs(p.xy); if(a2.y>a2.x) a2=a2.yx;
    float yy=a2.y-.06*clamp(floor(a2.y/.06+.5),0.,3.);
    d=U(d,float2(box3(float3(a2.x-.27,yy,p.z-.065),float3(.035,.012,.011),.005),6.));
#else
    // the bezel setting: a tapered cup out of the barrel, its lip over the girdle, six claws onto the crown
    float top=ZG6+.02;
    float body=max(box2(float2(r-.32,p.z-(top-.12)*.5),float2(.32,(top+.12)*.5),.008),(r-(.645-.38*(p.z-.08)))*.93);
    float k=.602, zc=GZD-.602*SSD-.004;
    float pocket=min(max(r-(SSD+.003),(zc+k*r-p.z)/sqrt(1.+k*k)), max(r-(SSD-.014),ZG6-.002-p.z));
    d=U(d,float2(max(body,-pocket),6.));
    float2 cq=prep(rotv(PI/6.,p.xy),6.);
    float3 a0=float3(.6,0.,ZG6-.05), a1=float3(.49,0.,ZG6+.066), pa=float3(cq,p.z)-a0, ba=a1-a0;
    float hc=saturate(dot(pa,ba)/dot(ba,ba));
    d=U(d,float2(length(pa-ba*hc)-(.044-.016*hc),6.));
#endif
  }
  return d;
#endif
}
float2 mapB(float3 p){                               // the reverse: a rim and a turned field with the name
  if(p.z>.06) return float2(p.z-.012,1.);
  float r=length(p.xy), dep=.19;
  float2 d=float2(ringS(p,.9,1.,-dep,.01,.025),1.);
  d=U(d,float2(disc(p,.905,-dep+.05,.01,.01),2.));
#if STONE == 3
  d.x=max(d.x,.705-r);                               // Emulsion: the ring is open right through
#endif
  return d;
}
float2 map(float3 p){
  if(p.z<-.06) return mapB(p);
  return U(mapF(p),mapB(p));
}
float3 nrm(float3 p){
  const float e=.0012*.5773; const float2 k=float2(1.,-1.);
  return normalize(k.xyy*map(p+k.xyy*e).x+k.yyx*map(p+k.yyx*e).x+k.yxy*map(p+k.yxy*e).x+k.xxx*map(p+k.xxx*e).x);
}
// march o + t d (coin space) from t to tmax; false on a miss. mn/tm: the closest pass (for the anti-aliased outline)
bool march(float3 o, float3 d, inout float t, float tmax, float eps, out float mn, out float tm){
  mn=1e9; tm=t;
  [loop] for(int i=0;i<140;i++){
    if(t>tmax) return false;
    float dd=map(o+d*t).x;
    if(dd<eps) return true;
    if(dd<mn){ mn=dd; tm=t; }
    t+=dd*.9;
  }
  return false;
}

#if STONE == 1 || STONE == 2
// the set stone, shaded like the full-view gem; light that leaves through the pavilion meets the polished cup
float3 stoneC(float3 o, float3 d, float tIn, float3 nIn, float3 M, out float cover, float pxw, float tOut){
  const bool ruby=STONE==2;
  float ior=ruby?1.77:2.417;
  float3 iorC=ruby?float3(1.762,1.77,1.778):float3(2.37,2.417,2.47);
  float3 absorb=ruby?float3(.12,3.4,2.7):float3(.004,.006,.01);
  cover=saturate((tOut-tIn)/(pxw*2.5));
  float3 p=o+d*tIn;
  float F=fres(-dot(d,nIn),ior);
  float3 col=v3(F*envG(toW(reflect(d,nIn))))*.8;
  float3 di=refr(d,nIn,1./ior);
  float3 thr=v3(1.-F);
  [loop] for(int b=0;b<6;b++){
    float a1,a2; float3 n1,n2;
    trav(p+di*1e-4,di,a1,n1,a2,n2);
    float L=max(a2,0.);
    thr*=exp(-absorb*L);
    p=p+di*L;
    float c=dot(di,n2);
    float3 outM=refr(di,-n2,ior);
    if(dot(outM,outM)<1e-4){ di=reflect(di,n2); continue; }
    float Fo=fres(c,ior);
    float3 e;
    if(n2.z<-.2){                                    // into the cup: polished metal sends it back up; darker deep in the seat
      float3 up=normalize(float3(outM.xy,abs(outM.z)));
      e=M*(.18+.8*envS(toW(up),uTime))*.95*lerp(.5,1.,smoothstep(-.6,.2,p.z));
    } else {
      float3 oR=refr(di,-n2,iorC.r), oG=refr(di,-n2,iorC.g), oB=refr(di,-n2,iorC.b);
      e.r=dot(oR,oR)>1e-4?envG(toW(oR)):0.; e.g=envG(toW(oG)); e.b=dot(oB,oB)>1e-4?envG(toW(oB)):0.;
    }
    col+=thr*(1.-Fo)*e;
    thr*=Fo;
    di=reflect(di,n2);
  }
  col+=thr*.04;
  if(ruby){
    col*=float3(1.,.55,.6);
    float g=.5+.5*sin(uTime*.8+dot(nIn,float3(3.1,1.7,2.3))*4.);
    col+=float3(.16,.005,.025)+float3(.5,.02,.06)*g*g*g*.35;
    col=pow(max(col,0.),1.15);
  } else { col=pow(max(col*.8,0.),1.7)*1.25+.01; }
  return 1.-exp(-max(col,0.)*1.6);
}
// is the stone in front along o + t d (coin space) before t = tHit? entry t, facet and exit t
bool stoneHit(float3 ro, float3 rd, float tHit, bool hit, out float3 og, out float gIn, out float3 gnIn, out float gOut){
  og=(ro-float3(0.,0.,GZD))/SSD;
  float3 gnOut; gIn=1.; gOut=0.; gnIn=float3(0.,0.,1.);
  float sb=dot(og,rd), sc2=dot(og,og)-1.1025;
  if(sb*sb-sc2>0.) trav(og,rd,gIn,gnIn,gOut,gnOut);
  return gIn<gOut && gOut>0. && (!hit || gIn*SSD<tHit);
}
#endif

// paint-filled engraving on the front: the ring text and the dial's scale
float paintF(float2 q, float r, float id, float aa, float lodR){
  float paint=0.;
  if(kR1.w>0.) paint=smoothstep(.1,.7,texM(uRing,q,1.,lodR));
  if(id>1.5&&id<2.5 && r>.71){
    float u=atan2(q.y,q.x)/TAU*60., fi=floor(u+.5), du=abs(u-fi)*TAU/60.*r; bool maj=modg(fi+60.,5.)<.5;
    float w=maj?.0065:.0035, r0=maj?.80:.84;
    paint=max(paint,smoothstep(w+aa,w,du)*smoothstep(r0-aa,r0,r)*smoothstep(.888,.884,r)*step(-.35,q.y/max(r,1e-3)));
  }
  return paint;
}
// what the dust needs to know about a spot: how sheltered (rec: the barrel, the scale, the grooves) and how flat (flt)
void spot(float id, float paint, float r, float3 p, float3 n, out float rec, out float flt){
  rec=(id>2.5&&id<3.5)?.7:(id>6.5&&id<7.5)?.15:0.;
  rec+=max(paint,0.)*.5+smoothstep(.03,0.,abs(r-.705))*.5+smoothstep(.03,0.,abs(r-.9))*.5;
  flt=abs(n.z)>.6?abs(n.z):lerp(.3,.8,smoothstep(0.,.9,p.y/max(length(p.xy),1e-4)));
}
// the haze part of ageLayers (cheap; the grains and the web come from the layer texture)
float3 ageHaze(float2 p, float recess, float flt){
  float dust=min(uWear.x,1.);
  float rc=min(recess,1.);
  float low=1.-smoothstep(-1.,1.,p.y+(wF(p*2.5+3.)-.5)*.3);
  float dl=pow(max(dust,1e-4),1.+2.*low*AGE0.z);
  float wall=1.-smoothstep(.72,.97,flt);
  float T=dl*lerp(1.,lerp(.05,.7,dl*dl),wall)*(1.+1.3*rc);
  return float3(smoothstep(0.,.6,T)*AGE0.w,(.17+.07*smoothstep(-1.,1.,p.y))*(1.-.12*rc),1.-.12*rc);
}
float4 ageL(float2 q, bool fr){
  float2 iu=saturate(q*.5+.5);
  float w, h; uAgeL.GetDimensions(w, h);
  float hx=.5/max(h,1.);                             // half a texel: never bleed across the front | back seam
  return uAgeL.SampleLevel(smp, float2((fr?0.:.5)+clamp(iu.x,hx,1.-hx)*.5, 1.-iu.y), 0.);
}
float3 ageOn(float3 col, float2 q, bool fr, float rec, float flt){
  if(kM2.w<=0.) return col;
  return ageApply(col,1.,ageHaze(q,rec,flt),ageL(q,fr));
}

float4 main(float4 sv : SV_Position) : SV_Target {
  gFrag = float2(sv.x - uOff.x, uRes.y - (sv.y - uOff.y));
  float S=min(uRes.x,uRes.y);
  { float a=.12*sin(uTime*.21); gER=float2(cos(a),sin(a)); }
#ifdef LAYER
  // the age layer L of one face, seen straight on (front: uLayer 1, back: 2), q in [-1, 1] across the item
  {
    float2 q=(gFrag-.5*uRes)/(.5*S);
    float aa=2./S;
    bool fr=uLayer<1.5;
    float3 ro=fr?float3(q,1.):float3(-q.x,q.y,-1.), rd=float3(0.,0.,fr?-1.:1.);
    float t=0., mn, tm;
    bool hit=march(ro,rd,t,2.,.0005,mn,tm);
    float rec=0., flt=1.;
#if STONE == 1 || STONE == 2
    float3 og, gnIn; float gIn, gOut;
    bool st=fr && stoneHit(ro,rd,t,hit,og,gIn,gnIn,gOut);
#else
    bool st=false;
#endif
    if(!st){
      if(!hit) return float4(0.,0.,0.,0.);
      float3 p=ro+rd*t;
      float id=map(p).y; float3 n=nrm(p);
      float r=length(q);
      float paint=(fr && n.z>.3)?paintF(q,r,id,aa,0.):0.;
      spot(id,paint,r,p,n,rec,flt);
    }
    float3 h; float4 L;
    if(!ageLayers(q,rec,flt,aa,h,L)) return float4(0.,0.,0.,0.);
    return L;
  }
#else
  float ext=kR0.w;
  float2 pp=(gFrag-.5*uRes)/(.5*S)*ext;
  float3 ow=float3(0.,0.,6.), dw=normalize(float3(pp,0.)-ow); float pxw=2.*ext/S;
  float3 ro=toC(ow), rd=toC(dw);
  float b=dot(ro,rd), c=dot(ro,ro)-1.44, h=b*b-c;
  if(h<0.) return float4(0.,0.,0.,0.);
  float t=max(-b-sqrt(h),0.), tmax=-b+sqrt(h);
  {                                                  // the metal fits in r < 1, -.2 < z < .36: clip the march to that cylinder
    float a2=dot(rd.xy,rd.xy), b2=dot(ro.xy,rd.xy), c2=dot(ro.xy,ro.xy)-1.;
    if(a2>1e-6){ float h2=b2*b2-a2*c2; if(h2<0.) tmax=-1.; else { h2=sqrt(h2); t=max(t,(-b2-h2)/a2); tmax=min(tmax,(-b2+h2)/a2); } }
    if(abs(rd.z)>1e-6){ float z0=(-.2-ro.z)/rd.z, z1=(.36-ro.z)/rd.z; t=max(t,min(z0,z1)); tmax=min(tmax,max(z0,z1)); }
  }
  float mn, tm;
  bool hit=march(ro,rd,t,tmax,max(.0005,pxw*.3),mn,tm);
  float cov=1.;
  if(!hit){                                          // just past the outline: shade the closest pass, partly covered
    cov=saturate(1.-mn/pxw);
    if(cov<=0.) cov=0.; else { t=tm; }
  }
  float3 M=uMetal;
  float scov=0.; float3 sc=float3(0.,0.,0.);
#if STONE == 1 || STONE == 2
  {
    float3 og, gnIn; float gIn, gOut;
    if(stoneHit(ro,rd,t,hit || cov>0.,og,gIn,gnIn,gOut)){
      sc=stoneC(og,rd,gIn,gnIn,M,scov,pxw/SSD,gOut);
      float3 ps=ro+rd*(gIn*SSD);
      sc=ageOn(sc,ps.xy,true,0.,1.);                 // dust settles on the stone too
      if(scov>.999 || cov<=0.) return float4(sc*scov,scov);
    }
  }
#endif
  if(cov<=0.) return float4(0.,0.,0.,0.);
  float3 p=ro+rd*t;
  float id=map(p).y;
  float3 n=nrm(p);
  float tt=uTime;
  bool fr=p.z>0.;
  float2 q=fr?p.xy:float2(-p.x,p.y);
  float r=length(q); float2 rq=r>1e-4?q/r:float2(1.,0.);
  float aa=pxw*1.2;
  if(id>5.5&&id<6.5) M=kM2.xyz;
  float3 nl=n;
  float matte=0., paint=0.;
  bool face=abs(n.z)>.6;
  if(length(p.xy)>.975 && abs(n.z)<.6){ float ang=atan2(p.y,p.x); float3 tg=float3(-sin(ang),cos(ang),0.); nl=normalize(nl+tg*sin(ang*220.)*.22); }   // the outer wall: fine reeding
#if STONE == 0
  {                                                  // the icon, etched bright into the die (on the field: engraved)
    const float kI=.17;
    if(fr && face && uIconOn>0. && r<kI*1.1){
      float lod=max(log2(512.*pxw/(2.*kI)),0.);
      float dd=1.4*pxw;
      float m=texM(uIcon,q,kI,lod);
      float gx=texM(uIcon,q+float2(dd,0.),kI,lod)-texM(uIcon,q-float2(dd,0.),kI,lod), gy=texM(uIcon,q+float2(0.,dd),kI,lod)-texM(uIcon,q-float2(0.,dd),kI,lod);
      float w=saturate(length(float2(gx,gy))*1.6);
      if(id>6.5&&id<7.5&&n.z>.7) paint=-smoothstep(.2,.8,m);
      else { nl=normalize(nl+float3(gx,gy,0.)*2.2*w); matte=smoothstep(.15,.85,m)*(1.-w); }
    }
  }
#endif
  float lodR=max(log2(1024.*pxw/2.),0.);
  if(!fr && face && kR2.w>0. && r<.9){               // the name on the reverse
    float lod=max(log2(1024.*pxw/(2.*.92)),0.);
    float dd=1.4*pxw;
    float m=texM(uBack,q,.92,lod);
    float gx=texM(uBack,q+float2(dd,0.),.92,lod)-texM(uBack,q-float2(dd,0.),.92,lod), gy=texM(uBack,q+float2(0.,dd),.92,lod)-texM(uBack,q-float2(0.,dd),.92,lod);
    float w=saturate(length(float2(gx,gy))*1.6);
    nl=normalize(nl+float3(-gx,gy,0.)*2.2*w); matte=max(matte,smoothstep(.15,.85,m)*(1.-w));
  }
  if(fr && n.z>.3) paint=max(paint,paintF(q,r,id,aa,lodR));
  float trace=0.;
#if STONE == 0
  if(fr&&id>3.5&&id<4.5){                            // traces from the die out to pads just inside the barrel
    const float r0=.3, r1=.54, j0=.4, j1=.46, tw=.0045, pr=.016;
    float an=TAU/24., a=atan2(q.y,q.x), k=floor((a+an*.5)/an), la=a-k*an;
    float jog=smoothstep(j0,j1,r)*an*.35*(modg(k,2.)<.5?1.:-1.);
    float dl=abs(r*sin(la-jog))-tw;
    float tr=smoothstep(aa,-aa,dl)*step(r0,r)*step(r,r1);
    float pad=smoothstep(pr+aa,pr,length(q-r1*float2(cos(k*an+jog),sin(k*an+jog))));
    trace=max(tr,pad);
  }
#endif
  float3 nw=toW(nl);
  float cv=saturate(-dot(nw,dw));
  float3 R=reflect(dw,nw);
#if STONE == 3
  { float ph=1.3*cv+atan2(q.y,q.x)/TAU*2.+r*1.2+.15*sin(uTime*.2); M=lerp(M,(.62+.38*cos(TAU*(ph+float3(0.,.33,.67))))*M,.42); }   // pearl: an oil sheen
#endif
  float fz=pow(1.-cv,5.);
  float3 F=M+(1.-M)*fz;
  float3 col;
  if(id>1.5&&id<2.5){                                // turned: light smeared round the centre
    float3 rw=toW(fr?float3(rq,0.):float3(-rq.x,rq.y,0.));
    float en=0.; [unroll] for(int j=0;j<4;j++){ float o=(float(j)-1.5)/1.5; en+=envS(reflect(dw,normalize(nw+rw*o*.28)),tt); }
    col=F*en*.25*.92;
  } else if(id>2.5&&id<3.5){                         // black anodised
    col=v3(.012)+(.04+.96*fz)*envS(R,tt)*.5;
  } else if(id>3.5&&id<4.5){                         // sandblasted
    col=M*(.07+.12*envS(nw,tt))*(.85+.3*hash(floor(gFrag)))+F*envS(R,tt)*.05;
  } else if(id>6.5&&id<7.5&&n.z>.7){                 // the die: dark silicon, a glassy sheen
    col=float3(.02,.024,.034)+(.05+.95*fz)*envS(R,tt)*.45;
  } else col=F*envS(R,tt);                           // polished
  if(trace>0.){ float3 G=kM2.xyz+(1.-kM2.xyz)*fz; col=lerp(col,G*envS(R,tt),trace); }
  if(matte>0.){ float grain=.85+.3*hash(floor(gFrag)); col=lerp(col,M*(.10+.10*envS(float3(-.35,.4,.85),tt))*grain,matte); }
  if(paint>0.) col=lerp(col,v3(.018),paint*.94);
  if(paint<0.){ float3 G=M+(1.-M)*fz; col=lerp(col,G*envS(R,tt),-paint); }
  col=aces(col*1.1);
  { float rec, flt; spot(id,paint,r,p,n,rec,flt); col=ageOn(col,q,fr,rec,flt); }   // age: dust in the barrel, the scale and the grooves
  float4 o4=float4(lerp(col,sc,scov),1.)*cov;
#if STONE == 3
  {                                                  // the bubble (drawn first, under this) shows wherever its sphere is in front
    float3 oc=ro-float3(0.,0.,ZF); float bb=dot(oc,rd), hh=bb*bb-dot(oc,oc)+RF*RF;
    if(hh>0. && -bb-sqrt(hh)<t) return float4(0.,0.,0.,0.);
  }
#endif
  return o4;
#endif
}
"#;

/// Source of one coin pixel shader: `stone` 0 die, 1 Diamond, 2 Ruby, 3 Emulsion; `layer` = the age-layer program.
pub fn coin_src(stone: u8, layer: bool) -> String {
    let age = {
        let a = GEM.find("// ---- age: dust and cobwebs").unwrap_or(0);
        let b = GEM[a..].find("float4 main(").map_or(GEM.len(), |i| a + i);
        &GEM[a..b]
    };
    let mut s = String::with_capacity(COMMON.len() + age.len() + COIN.len() + 64);
    s.push_str(&format!("#define STONE {stone}\n"));
    if layer {
        s.push_str("#define LAYER 1\n");
    }
    s.push_str(COMMON);
    s.push_str(age);
    s.push_str(COIN);
    s
}
