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
  float  uP0; float uP1; float uP2; float uP3;     // spare
};
SamplerState smp : register(s0);
Texture2D<float> uIcon : register(t0);     // 128x128 alpha (white-on-clear icon), mip chain
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

/// Metals, Diamond and Ruby (gem.js) as Uptime Medals: reference shape "round" (shape 0). Metals = `medal` (struck
/// medal: polished raised rim, spun face with fine rings, icon engraved with a frosted floor and bright cut walls);
/// Diamond / Ruby = `brill` (round brilliant, ik = -1: no etching on the stone). `envM`, `env`, `inner` as in gem.js.
/// Only change: the viewer tilt (`uTilt`, 0 by default) also moves the medal's view vector, as it does for vplaque.
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

// icon mask: 128 px square, half size .66 (badge units) centred on the medal; mip chosen from the on-screen size
float icA(float2 p){
  float2 iu = p/.66*.5 + .5;
  if (iu.x < 0. || iu.y < 0. || iu.x > 1. || iu.y > 1.) return 0.;
  float lod = max(log2(128.*1.04/(.66*max(uPx, 8.))), 0.);
  return uIcon.SampleLevel(smp, float2(iu.x, 1. - iu.y), lod);
}
// ---- struck metal medals (tiers 1-4): polished raised rim, spun face with fine rings, icon engraved (frosted floor, bright cut walls)
float4 medal(float2 p, float r, float aa, float t){
  float3 base = uMetal;
  float3 v = normalize(float3(-p*.32 + float2(.06*sin(t*.5), .05*sin(t*.37 + 1.)) + uTilt*.9, 1.));   // a near camera: the light slides across the face
  float2 rd = r > 1e-4 ? p/r : float2(1., 0.);
  float3 n = float3(0., 0., 1.);
  float RIM = .83;
  float rough = 1.;
  if (r > RIM){
    float x = clamp((r - RIM)/(.975 - RIM), 0., 1.);
    float sl = cos(x*PI)*1.1;                        // rounded raised rim
    sl = lerp(sl, -2.4, smoothstep(.972, .99, r));   // and a crisp outer edge
    n = normalize(float3(-rd*sl, 1.));
    rough = 0.;
  } else {
    // spun face: very fine concentric rings
    float ring = sin(r*uPx*.9)*.5 + .5;
    n = normalize(float3(rd*(ring - .5)*.04, 1.));
  }
  float3 R = reflect(-v, n);
  float e = envM(R, t);
  // spun metal: a soft bright streak through the middle, lined up with the main light (anisotropic sheen)
  float la = t*.25 + atan2(.45, .5);
  float2 L2 = float2(cos(la), sin(la));
  float sheen = pow(abs(dot(rd, L2)), 14.)*smoothstep(.0, .25, r)*(1. - rough*0.) + .0;
  float la2 = t*.25 + atan2(-.25, -.65);
  sheen += .5*pow(abs(dot(rd, float2(cos(la2), sin(la2)))), 18.)*smoothstep(.0, .25, r);
  float3 col;
  if (r > RIM){ col = base*e*1.25 + v3(e*e*.04); }
  else { col = base*(e*.85 + sheen*.55*(1. - r*.3)); }
  // a fine groove where the rim meets the face
  float gr = smoothstep(aa*1.5, 0., abs(r - RIM));
  col *= 1. - gr*.55;
  // engraved icon: sunken, sandblasted floor (matte) and polished walls that catch the light
  if (uIconOn > 0. && r < RIM){
    float d = 1.6/max(uPx, 40.);
    float m = icA(p);
    float gx = icA(p + float2(d, 0.)) - icA(p - float2(d, 0.));
    float gy = icA(p + float2(0., d)) - icA(p - float2(0., d));
    if (m + abs(gx) + abs(gy) > 0.){
      float grain = .82 + .36*hash(floor(gFrag));
      float3 floorC = base*(.40 + .14*envM(float3(0., 0., 1.), t))*grain;
      col = lerp(col, floorC, smoothstep(.15, .85, m));
      float3 nw = normalize(float3(float2(gx, gy)*2.2, 1.));
      float w = clamp(length(float2(gx, gy))*1.6, 0., 1.);
      float ew = envM(reflect(-v, nw), t);
      col = lerp(col, base*ew*1.3, w*.9);
    }
  }
  float inside = smoothstep(1. + aa, 1. - aa, r);
  col = 1. - exp(-col*1.6);
  return float4(col*inside, inside);
}

float4 main(float4 sv : SV_Position) : SV_Target {
  gFrag = float2(sv.x - uOff.x, uRes.y - (sv.y - uOff.y));
  float S = min(uRes.x, uRes.y);
  float2 p = (gFrag - .5*uRes)/(.5*S)*1.04;
  float r = length(p);
  float aa = 2./S*1.04;
  float t = uTime + uSeed;
  if (r > 1. + aa) return float4(0., 0., 0., 0.);
  if (uKind > 1.5) return medal(p, r, aa, t);
  return brill(p, r, aa, t);
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
