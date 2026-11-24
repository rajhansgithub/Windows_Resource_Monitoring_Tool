/* Real 3D for the full view (v4+): Diamond and Ruby are ray traced as a round brilliant (73 facets: table, 8 bezel, 8 star,
   16 upper girdle, 16 girdle, 8 pavilion mains, 16 lower girdle). Light enters through the crown, bounces inside off the
   pavilion (total internal reflection), and leaves; diamond splits colour at the exit (fire), ruby absorbs green and blue along
   the path. Emulsion is a sphere of soap film: interference colour on the front and on the inside of the back wall, the film
   thinner at the top (it drains), and the Resource Monitor pulse floating inside.
   One canvas at a time (the full view). YTPM3D.start(canvas, 'dia'|'ruby'|'bubble'); YTPM3D.rot(yaw, pitch, spin) in degrees;
   YTPM3D.stop(). No WebGL: start() returns false and the caller keeps the flat medal. */
(function (root) {
  'use strict';
  const VS = 'attribute vec2 a;void main(){gl_Position=vec4(a,0.,1.);}';
  const D2R = Math.PI / 180;

  /* ---- brilliant geometry (girdle radius 1, centred in z): table, 8 bezel, 8 star, 16 upper girdle, 16 girdle, 8 pavilion mains,
     16 lower girdle = 73 planes, walked in one loop over the 16 sectors (8-fold symmetry), so the shader stays small */
  function geo() {
    const gt = 0.02, gb = -0.02, rt = 0.56, cA = 34.5 * D2R, pA = 40.75 * D2R, sA = 21 * D2R, ugA = 42 * D2R, lgA = 42 * D2R;
    const zTab = gt + (1 - rt) * Math.tan(cA), zCul = gb - Math.tan(pA), zc = (zTab + zCul) / 2;
    // a plane tilted by th at azimuth phi through the point at radius r, height z: c = sin(th)*r + (+/-cos(th))*(z - zc)
    const cUp = (th, r, z) => Math.sin(th) * r + Math.cos(th) * (z - zc), cDn = (th, r, z) => Math.sin(th) * r - Math.cos(th) * (z - zc);
    const n = (x) => x.toFixed(6);
    return `
const float TABC=${n(zTab - zc)};
const vec2 BEZ=vec2(${n(Math.sin(cA))},${n(Math.cos(cA))}); const float BEZC=${n(cUp(cA, 1, gt))};
const vec2 STA=vec2(${n(Math.sin(sA))},${n(Math.cos(sA))}); const float STAC=${n(cUp(sA, rt / Math.cos(Math.PI / 8), zTab))};
const vec2 UGI=vec2(${n(Math.sin(ugA))},${n(Math.cos(ugA))}); const float UGIC=${n(cUp(ugA, 1, gt))};
const vec2 PAV=vec2(${n(Math.sin(pA))},${n(-Math.cos(pA))}); const float PAVC=${n(cDn(pA, 1, gb))};
const vec2 LGI=vec2(${n(Math.sin(lgA))},${n(-Math.cos(lgA))}); const float LGIC=${n(cDn(lgA, 1, gb))};
const vec2 ROT8=vec2(${n(Math.cos(Math.PI / 8))},${n(Math.sin(Math.PI / 8))}); const vec2 HALF=vec2(${n(Math.cos(Math.PI / 16))},${n(Math.sin(Math.PI / 16))});`;
  }

  const COMMON = `
precision highp float;
uniform vec2 uRes; uniform float uTime; uniform mat3 uR; uniform mat3 uRT; uniform float uKind;
const float PI=3.14159265; const float TAU=6.2831853;
float hash(vec2 p){ p=fract(p*vec2(123.34,456.21)); p+=dot(p,p+45.32); return fract(p.x*p.y); }
mat2 rot(float a){ float c=cos(a), s=sin(a); return mat2(c,-s,s,c); }
// eye at z=6 looking at the origin: a mild perspective so turning reads as 3D
void cam(out vec3 o, out vec3 d, out float pxw, float ext){
  float S=min(uRes.x,uRes.y);
  vec2 p=(gl_FragCoord.xy-.5*uRes)/(.5*S)*ext;
  o=vec3(0.,0.,6.); d=normalize(vec3(p,0.)-o); pxw=2.*ext/S;
}`;

  const GEM = COMMON + (root.YTPMWearGLSL || 'uniform vec3 uWear; uniform vec4 uAge[4]; vec3 aged(vec3 c,float a,vec2 p,float r,float f,float m,float aa){return c;}') + `
// studio (world space, +z = towards the viewer): soft ceiling, three strip lights, hard spots, and the viewer's dark head
float env(vec3 d){
  d.xy=rot(uTime*.05)*d.xy;
  float az=atan(d.y,d.x);
  float s=.03+.45*smoothstep(-.1,.9,d.z);
  s+=1.1*pow(max(cos(az*3.)*.5+.5,0.),5.)*smoothstep(-.2,.5,d.z)*smoothstep(.98,.75,d.z);
  s+=pow(max(dot(d,normalize(vec3(.55,.55,.63))),0.),140.)*8.;
  s+=pow(max(dot(d,normalize(vec3(-.72,.25,.64))),0.),100.)*5.;
  s+=pow(max(dot(d,normalize(vec3(.05,-.78,.62))),0.),180.)*7.;
  s+=pow(max(dot(d,normalize(vec3(.3,.1,-.95))),0.),30.)*1.2;    // a light behind, seen through the stone when it is turned
  s+=1.5*smoothstep(.72,.9,d.z)*smoothstep(.99,.95,d.z)*(.75+.25*cos(az*8.));   // a ring light round the viewer, like a jeweller's lamp
  s*=1.-.9*smoothstep(.975,.995,d.z);                             // the viewer's head: small and dark, straight in front
  return s;
}
${geo()}
#define P(N,C) { vec3 nn=N; float dn=dot(nn,d); float tt=(C-dot(nn,o))/dn; if(dn<0.){ if(tt>tIn){ tIn=tt; nIn=nn; } } else { if(tt<tOut){ tOut=tt; nOut=nn; } } }
// ray o + t d against the stone (a convex solid: inside every plane): entry t and facet, exit t and facet
void trav(vec3 o, vec3 d, out float tIn, out vec3 nIn, out float tOut, out vec3 nOut){
  tIn=-1e9; tOut=1e9; nIn=vec3(0.,0.,1.); nOut=vec3(0.,0.,1.);
  P(vec3(0.,0.,1.),TABC)
  vec2 cs=vec2(1.,0.);                                  // cos, sin of the sector angle k*pi/8
  for(int k=0;k<16;k++){
    vec2 ch=vec2(cs.x*HALF.x-cs.y*HALF.y, cs.y*HALF.x+cs.x*HALF.y);   // k*pi/8 + pi/16
    P(vec3(cs,0.),1.)                                   // girdle
    P(vec3(ch*UGI.x,UGI.y),UGIC)                        // upper girdle
    P(vec3(ch*LGI.x,LGI.y),LGIC)                        // lower girdle
    if(k==2*(k/2)){ P(vec3(cs*BEZ.x,BEZ.y),BEZC) P(vec3(cs*PAV.x,PAV.y),PAVC) }   // bezel + pavilion main
    else { P(vec3(cs*STA.x,STA.y),STAC) }               // star
    cs=vec2(cs.x*ROT8.x-cs.y*ROT8.y, cs.y*ROT8.x+cs.x*ROT8.y);
  }
}
float fres(float c, float ior){ float F0=pow((ior-1.)/(ior+1.),2.); return F0+(1.-F0)*pow(1.-clamp(c,0.,1.),5.); }
void main(){
  vec3 ow, dw; float pxw; cam(ow,dw,pxw,1.16);
  vec3 o=uRT*ow, d=uRT*dw;                      // into the stone's own space
  float tIn, tOut; vec3 nIn, nOut;
  trav(o,d,tIn,nIn,tOut,nOut);
  if(tIn>=tOut || tOut<0.){ gl_FragColor=vec4(0.); return; }
  float cover=clamp((tOut-tIn)/(pxw*2.5),0.,1.);
  bool ruby=uKind>.5;
  float ior=ruby?1.77:2.417;
  vec3 iorC=ruby?vec3(1.762,1.77,1.778):vec3(2.37,2.417,2.47);   // diamond fire a touch stronger than life so it reads on screen
  vec3 absorb=ruby?vec3(.12,3.4,2.7):vec3(.004,.006,.01);
  vec3 p=o+d*tIn;
  vec2 pe=p.xy;
  float ci=-dot(d,nIn);
  float F=fres(ci,ior);
  vec3 col=vec3(F*env(uR*reflect(d,nIn)))*.8;
  vec3 di=refract(d,nIn,1./ior);
  vec3 thr=vec3(1.-F);
  for(int b=0;b<6;b++){
    float a1,a2; vec3 n1,n2;
    trav(p+di*1e-4,di,a1,n1,a2,n2);
    float L=max(a2,0.);
    thr*=exp(-absorb*L);
    p=p+di*L;
    float c=dot(di,n2);                         // leaving through the facet with outward normal n2
    vec3 outM=refract(di,-n2,ior);
    if(dot(outM,outM)<1e-4){ di=reflect(di,n2); continue; }   // total internal reflection: stays inside
    float Fo=fres(c,ior);
    vec3 e;
    vec3 oR=refract(di,-n2,iorC.r), oG=refract(di,-n2,iorC.g), oB=refract(di,-n2,iorC.b);
    e.r=dot(oR,oR)>1e-4?env(uR*oR):0.;
    e.g=env(uR*oG);
    e.b=dot(oB,oB)>1e-4?env(uR*oB):0.;
    col+=thr*(1.-Fo)*e;
    thr*=Fo;
    di=reflect(di,n2);
  }
  col+=thr*.04;
  if(ruby){
    col*=vec3(1.,.55,.6);
    col+=vec3(.16,.005,.025)+vec3(.5,.02,.06)*pow(.5+.5*sin(uTime*.8+dot(nIn,vec3(3.1,1.7,2.3))*4.),3.)*.35;   // its slow red glow
    col=pow(col,vec3(1.15));
  } else {
    col=pow(col*.8,vec3(1.7))*1.25+.01;          // a brilliant is mostly bright white and near black
  }
  col=1.-exp(-max(col,0.)*1.6);
  col=aged(col,1.,pe,smoothstep(.86,1.,length(pe))*.6,1.-.3*smoothstep(.5,1.,length(pe)),0.,pxw);
  gl_FragColor=vec4(col*cover,cover);
}`;

  const BUBBLE = COMMON + `
#ifndef BEXT
#define BEXT 1.03
#endif
uniform float uEdgeW, uEdgeVary, uEdgeCol, uSat, uFilmIn, uRefl, uFlow, uThick, uRim, uGlow, uBreath, uMotAmt, uMotT, uDotRefr, uDotRing, uRefrT;   // the look (YTPMLook)
uniform float uLqRefr, uLqRefl, uLqFilm, uLqTint, uLqWob, uLqDrops, uLqDropS, uLqMove;   // the liquid pulse
float h3(vec3 p){ p=fract(p*vec3(.1031,.1030,.0973)); p+=dot(p,p.yxz+33.33); return fract((p.x+p.y)*p.z); }
float n3(vec3 p){ vec3 i=floor(p), f=fract(p); f=f*f*(3.-2.*f);
  return mix(mix(mix(h3(i),h3(i+vec3(1,0,0)),f.x),mix(h3(i+vec3(0,1,0)),h3(i+vec3(1,1,0)),f.x),f.y),
             mix(mix(h3(i+vec3(0,0,1)),h3(i+vec3(1,0,1)),f.x),mix(h3(i+vec3(0,1,1)),h3(i+vec3(1,1,1)),f.x),f.y),f.z); }
float fbm3(vec3 p){ float v=0., a=.5; for(int i=0;i<5;i++){ v+=a*n3(p); p=p*2.03+vec3(1.7,9.2,3.1); a*=.5; } return v; }
vec3 wl(float l){ return vec3(exp(-pow((l-605.)/48.,2.)) + .28*exp(-pow((l-425.)/28.,2.)), exp(-pow((l-545.)/42.,2.)), exp(-pow((l-455.)/32.,2.))); }
vec3 film(float d, float cosT){
  vec3 c=vec3(0.), s=vec3(0.);
  for(int i=0;i<8;i++){ float l=400.+float(i)*42.857; vec3 w=wl(l); float ph=TAU*2.*1.33*d*cosT/l; c+=w*(.5-.5*cos(ph)); s+=w; }
  vec3 f=2.*c/s; float y=dot(f,vec3(.299,.587,.114));
  return mix(vec3(y),f,uSat);
}
// film thickness (nm) at a point of the sphere: swirls that turn with the bubble, thinner at the top where the film drains
float thick(vec3 pw){
  vec3 q=uRT*pw;
  float ft=uTime*.06*uFlow;
  vec3 w=vec3(fbm3(q*1.1+vec3(0.,ft,0.)), fbm3(q*1.1+vec3(5.2,1.3,-ft)), 0.);
  float n=fbm3(q*1.4+w*1.3+vec3(ft*.4,-ft*.3,ft*.2));
  return max(uThick+420.*(n-.5)-120.*pw.y, 20.);   // same range as the flat badge
}
// studio: a big soft window up left, a floor glow, a small hard light; +z = towards the viewer
float env(vec3 r){
  vec3 wd=normalize(vec3(-.45,.55,.7));
  vec3 u=normalize(cross(wd,vec3(0.,0.,1.))+vec3(0.,1e-4,0.)), v=cross(u,wd);
  vec2 q=vec2(dot(r,u),dot(r,v))/max(dot(r,wd),.05);
  float win=smoothstep(.16,0.,length(max(abs(q)-vec2(.16,.11),0.))-.04)*step(0.,dot(r,wd));   // a soft, rounded window
  float floorL=smoothstep(-.1,-.8,r.y)*.55;
  float spot=pow(max(dot(r,normalize(vec3(.6,.35,.72))),0.),300.)*6.;
  return .05+1.15*win+floorL+spot;
}
float seg(vec2 p, vec2 a, vec2 b){ vec2 pa=p-a, ba=b-a; float h=clamp(dot(pa,ba)/dot(ba,ba),0.,1.); return length(pa-ba*h); }
// the Resource Monitor pulse, floating in the middle of the bubble (same mark as the badge)
float logo(vec2 q, float aa){
  q/=1.+uMotAmt*.05*sin(uTime*TAU/uMotT);
  vec2 p=vec2(12.+q.x*9., 9.4-q.y*9.);
  float a=aa*9.;
  float pl=min(min(min(seg(p,vec2(4.,9.8),vec2(7.6,9.8)),seg(p,vec2(7.6,9.8),vec2(9.6,6.1))),min(seg(p,vec2(9.6,6.1),vec2(11.4,13.4)),seg(p,vec2(11.4,13.4),vec2(13.2,4.8)))),min(seg(p,vec2(13.2,4.8),vec2(14.9,9.8)),seg(p,vec2(14.9,9.8),vec2(20.,9.8))))-.95;
  return smoothstep(a,-a,pl);
}
vec3 glow(vec2 q, float k){
  vec3 g=vec3(0.);
  g+=vec3(.18,.45,1.)*exp(-dot((q-vec2(0.,.32))*vec2(1.3,2.4),(q-vec2(0.,.32))*vec2(1.3,2.4)))*.75;
  g+=vec3(.15,.77,1.)*exp(-dot((q-vec2(.28,.2))*vec2(2.,3.),(q-vec2(.28,.2))*vec2(2.,3.)))*.5;
  g+=vec3(.7,.3,1.)*exp(-dot((q-vec2(-.42,-.02))*vec2(2.4,3.6),(q-vec2(-.42,-.02))*vec2(2.4,3.6)))*.45;
  g+=vec3(1.)*exp(-dot(q*vec2(1.8,3.2),q*vec2(1.8,3.2)))*.8;
  g+=vec3(1.,.82,.25)*exp(-dot((q-vec2(0.,-.3))*vec2(1.4,2.6),(q-vec2(0.,-.3))*vec2(1.4,2.6)))*.7;
  g+=vec3(1.,.36,.16)*exp(-dot((q-vec2(.45,-.2))*vec2(2.4,3.4),(q-vec2(.45,-.2))*vec2(2.4,3.4)))*.55;
  return g*k*uGlow;
}
vec3 mark(vec2 q, float k, float aa){
  float y=q.y;
  vec3 dc=mix(vec3(1.,.42,.23), vec3(1.,.86,.45), smoothstep(-.9,-.3,y));
  dc=mix(dc, vec3(1.), smoothstep(-.25,0.,y)); dc=mix(dc, vec3(.55,.85,1.), smoothstep(.1,.45,y)); dc=mix(dc, vec3(.31,.55,1.), smoothstep(.5,.95,y));
  return glow(q,k)*.55 + logo(q,aa)*dc*(1.-.15*k);
}
vec3 dcol(float y){   // the pulse's colour by height (mark units)
  vec3 dc=mix(vec3(1.,.42,.23), vec3(1.,.86,.45), smoothstep(-.9,-.3,y));
  dc=mix(dc, vec3(1.), smoothstep(-.25,0.,y)); dc=mix(dc, vec3(.55,.85,1.), smoothstep(.1,.45,y)); return mix(dc, vec3(.31,.55,1.), smoothstep(.5,.95,y));
}
// ---- the pulse as a liquid: a tube of liquid in the shape of the mark, floating inside the sphere (view space: it never turns)
float sg3(vec3 p, vec3 a, vec3 b){ vec3 pa=p-a, ba=b-a; float h=clamp(dot(pa,ba)/dot(ba,ba),0.,1.); return length(pa-ba*h); }
float smn(float a, float b, float k){ float h=clamp(.5+.5*(b-a)/k,0.,1.); return mix(b,a,h)-k*h*(1.-h); }
// tiny bubbles that belong to the pulse: fixed places round the line (same in the flat badge), drifting a little
vec4 drop(float fi, float t){
  vec3 c=vec3(-.6+fi*.105+.03*sin(fi*7.3), .30*sin(fi*2.39+.5)-.03, .08*sin(fi*1.7));
  c+=.014*uLqMove*vec3(sin(t*.5+fi), sin(t*.43+fi*2.), sin(t*.31+fi*1.3));
  return vec4(c,(.013+.026*fract(fi*.618))*uLqDropS);
}
float liq(vec3 p){
  float t=uTime;
  // alive, but calm: the pulse drifts and tilts very slightly
  p.y-=.012*uLqMove*sin(t*.6);
  p.xy=rot(.035*uLqMove*sin(t*.37))*p.xy;
  float sc=1.+uMotAmt*.05*sin(t*TAU/uMotT);
  p/=sc;
  vec3 A=vec3(-.5511,-.0276,0.), B=vec3(-.3031,-.0276,0.), C=vec3(-.1653,.2273,0.), D=vec3(-.0413,-.2756,0.), E=vec3(.0827,.3169,0.), F=vec3(.1998,-.0276,0.), G=vec3(.5511,-.0276,0.);
  float d=sg3(p,A,B);
  d=smn(d,sg3(p,B,C),.035); d=smn(d,sg3(p,C,D),.035); d=smn(d,sg3(p,D,E),.035); d=smn(d,sg3(p,E,F),.035); d=smn(d,sg3(p,F,G),.035);
  float rad=.062+(.006*sin(p.x*14.-t*2.1)+.003*sin(p.x*33.+p.y*9.+t*3.3))*uLqWob*2.;
  d-=rad;
  for(int i=0;i<12;i++){
    float fi=float(i);
    if(fi>=12.*uLqDrops) break;
    vec4 q=drop(fi,t);
    d=smn(d,length(p-q.xyz)-q.w,.012);
  }
  return d*sc;
}
vec3 liqN(vec3 p){
  const vec2 e=vec2(.0015,-.0015);
  return normalize(e.xyy*liq(p+e.xyy)+e.yyx*liq(p+e.yyx)+e.yxy*liq(p+e.yxy)+e.xxx*liq(p+e.xxx));
}
void main(){
  vec3 o, d; float pxw; cam(o,d,pxw,BEXT);
  float b=dot(o,d), c=dot(o,o)-1., h=b*b-c;
  if(h<=0.){ gl_FragColor=vec4(0.); return; }
  float sh=sqrt(h);
  float cover=clamp(sh/(pxw*2.2),0.,1.);
  vec3 p1=o+d*(-b-sh), p2=o+d*(-b+sh);
  vec3 n1=normalize(p1), n2=normalize(p2);
  float k=.62+.38*sin(uTime*TAU/uBreath);
  // front film: Fresnel reflection, coloured by interference
  float c1=clamp(-dot(d,n1),0.,1.);
  // edge band: wider or narrower (edge width), uneven round the outline (unevenness), stronger colour (edge colour)
  // edge band exactly as the flat badge: Fresnel squeezed into a band of width wf (in outline radii), wf varying round the edge
  float ftb=uTime*uFlow;
  vec3 qn=uRT*n1;
  float bw=fbm3(qn*1.7+vec3(ftb*.035,-ftb*.028,0.))+.35*fbm3(qn*4.1-vec3(ftb*.05,ftb*.04,0.));
  float wf=mix(1.,mix(.14,3.2,smoothstep(.45,.95,bw)),uEdgeVary)*uEdgeW;
  float rS=sqrt(max(1.-c1*c1,0.));
  float rB=clamp(1.-(1.-rS)/max(wf,.01),0.,.9999);
  float F1=.02+.98*pow(1.-sqrt(1.-rB*rB),5.);
  float ct1=sqrt(1.-(1.-c1*c1)/1.77);
  vec3 f1=film(thick(p1),ct1);
  // same light model as the flat badge: a dark, clear middle (film colour only faintly, "film colour in the middle")
  // and the colour carried by the Fresnel edge band; reflections only brighten what the film already shows
  float e1=min(env(reflect(d,n1)),1.65);   // flat badge range: no hard glints in the clear middle
  vec3 col=f1*(uFilmIn*.05+uEdgeCol*2.6*F1)*(.25+uRefl*(e1-.05));
  // the inside of the back wall: a second, upside-down reflection, dimmer (the light crossed the front film twice)
  float c2=clamp(dot(d,n2),0.,1.);
  float rS2=sqrt(max(1.-c2*c2,0.)), rB2=clamp(1.-(1.-rS2)/max(wf,.01),0.,.9999);
  float F2=.02+.98*pow(1.-sqrt(1.-rB2*rB2),5.);
  float ct2=sqrt(1.-(1.-c2*c2)/1.77);
  vec3 f2=film(thick(p2),ct2);
  vec3 back=f2*(uFilmIn*.03+uEdgeCol*1.4*F2)*(.25+uRefl*(min(env(reflect(d,-n2)),1.65)-.05))*.6;
  float tz=(0.-o.z)/d.z; vec2 q0=(o+d*tz).xy;
  // the pulse inside: a tube of liquid, ray-marched in view space: it always faces you while the bubble (its film) turns
  vec3 ro=p1, rdo=d;
  float tmax=2.*sh, tt=0., hmin=1e3; bool hit=false;
  // only march where the ray passes the pulse's box
  vec3 bi=1./rdo, t0=(-vec3(.75,.5,.2)-ro)*bi, t1=(vec3(.75,.5,.2)-ro)*bi;
  vec3 tn=min(t0,t1), tf=max(t0,t1);
  float ta=max(max(tn.x,tn.y),max(tn.z,0.)), tb=min(min(tf.x,tf.y),min(tf.z,tmax));
  vec3 inner=vec3(0.);
  if(ta<tb){
    tt=ta;
    for(int i=0;i<48;i++){
      float hh=liq(ro+rdo*tt);
      hmin=min(hmin,hh);
      if(hh<.0007){ hit=true; break; }
      tt+=hh*.9;
      if(tt>tb) break;
    }
  }
  // soft light round the pulse (centre light, breathing), facing you too
  vec3 ph=ro+rdo*clamp(-ro.z/(abs(rdo.z)>1e-3?rdo.z:1e-3),0.,tmax);
  inner+=glow(ph.xy/.62,k)*.38*smoothstep(.0,.35,abs(rdo.z));
  if(hit){
    // clear water: mostly what is behind it, bent (refraction), dark where the surface turns away (light leaves the
    // other way), the studio on its surface (reflection), a hint of soap-film colour, and a little of the logo's colour
    vec3 pp=ro+rdo*tt, no=liqN(pp), nw=no, pw=pp;
    float ndv=clamp(-dot(nw,d),0.,1.);
    float Fl=.02+.98*pow(1.-ndv,5.);
    vec3 R=reflect(d,nw);
    vec3 rr=normalize(mix(d,refract(d,nw,1./1.33),clamp(uLqRefr*2.,0.,1.)));
    float bb=dot(pw,rr), cc=dot(pw,pw)-1., hb=max(bb*bb-cc,0.);
    vec3 pb=pw+rr*(-bb+sqrt(hb)), nb=normalize(pb);
    float cb=clamp(dot(rr,nb),0.,1.);
    vec3 behind=film(thick(pb),sqrt(1.-(1.-cb*cb)/1.77))*(uFilmIn*.03+uEdgeCol*1.4*(.02+.98*pow(1.-cb,5.)))*.6;
    vec3 ro2=pw, rd2=rr;
    vec3 pg=ro2+rd2*clamp(-ro2.z/(abs(rd2.z)>1e-3?rd2.z:1e-3),0.,2.);
    behind+=glow(pg.xy/.62,k)*.38+min(env(rr),2.)*.05;
    float edge=mix(1.,smoothstep(.05,.55,ndv),clamp(uLqRefr*1.6,0.,1.));
    vec3 trans=behind*(1.-Fl)*edge;
    float spec=(min(env(R),3.)*Fl*1.8+pow(max(dot(R,normalize(vec3(-.45,.55,.7))),0.),120.)*2.4+pow(max(dot(R,normalize(vec3(.6,.35,.72))),0.),400.)*2.)*uLqRefl*2.;
    vec3 fl=film(uThick*.5+220.*nw.y+60.*sin(pp.x*9.+uTime*.4),ndv)*(Fl*2.2+.03)*uLqFilm*1.6;
    vec3 tint=dcol(pp.y/.62)*(.08+.5*pow(ndv,2.))*uLqTint*(.85+.25*k);
    inner=trans+vec3(spec)+fl+tint;
    back=vec3(0.);
  } else {
    inner+=vec3(.8,.9,1.)*exp(-hmin*70.)*.05*uLqRefl;   // a faint glint where a ray only grazes the water
  }
  col+=(back+inner*.95)*(1.-F1);
  // the mark's refraction copies near the rim, as on the flat badge (screen space: p = position on the outline disc, radius 1)
  vec2 pd=q0/1.0142; float rd=length(pd);
  float aa=pxw*1.6/.62;
  float sway=.26*sin(uTime*TAU/uRefrT)+.1*sin(uTime*TAU/(uRefrT*1.45));
  if(uDotRing>0.){
    float wr=smoothstep(.8,.88,rd)*smoothstep(1.,.96,rd);
    if(wr>0.){
      float s=(rd-.8)/.195; float rr=(1.-s)*1.02;
      vec2 dir=-normalize(pd)*rot(sway);
      float cr=mark(dir*rr*1.05,k,aa*3.).r, cg=mark(dir*rr,k,aa*3.).g, cb=mark(dir*rr*.95,k,aa*3.).b;
      col+=vec3(cr,cg,cb)*wr*.3*uDotRing*(.75+.25*k);
    }
  }
  if(uDotRefr>0.){
    for(int j=0;j<2;j++){
      float side=j==0?-1.:1.;
      float a2=(j==0? 3.1416 : 0.) + side*(.16*sin(uTime*TAU/uRefrT+float(j)*1.7)+.06*sin(uTime*TAU/(uRefrT*1.41))) - .1;
      vec2 nrm=vec2(cos(a2),sin(a2)); vec2 tng=vec2(-nrm.y,nrm.x);
      vec2 u=pd-nrm*.8; vec2 lu=vec2(dot(u,tng), dot(u,nrm)*1.9);
      float sz=.19; vec2 qq=-lu/sz;
      float m=smoothstep(1.08,.8,length(qq))*smoothstep(.995,.94,rd);
      if(m>0.){
        float aa3=aa*.62/sz*2.;
        float cr=mark(qq*1.06,k,aa3).r, cg=mark(qq,k,aa3).g, cb=mark(qq*.94,k,aa3).b;
        col+=vec3(cr,cg,cb)*m*uDotRefr*(.7+.3*k);
      }
    }
  }
  // a thin bright rim where the film is seen edge-on
  col+=f1*smoothstep(.25,0.,c1)*.35*uRim/.3;
  col=1.-exp(-col*1.15);
  float a=clamp(max(col.r,max(col.g,col.b))*1.25,.06,1.);
  gl_FragColor=vec4(col*cover,a*cover);
}`;


  // ---- a struck metal medal as a real object: a thick disc with a rounded edge, reeded rim, the icon engraved on the
  // front, the name engraved on the back. Same studio and metal as the flat medal, plus age.
  const MEDAL = COMMON + (root.YTPMWearGLSL || 'uniform vec3 uWear; uniform vec4 uAge[4]; vec3 aged(vec3 c,float a,vec2 p,float r,float f,float m,float aa){return c;}') + `
uniform vec3 uMetal; uniform sampler2D uIcon; uniform float uIconOn; uniform sampler2D uBack; uniform float uBackOn;
float envS(vec3 d, float t){
  d.xy=rot(.12*sin(t*.21))*d.xy;
  float s=.09+.12*smoothstep(-.2,.9,d.y);
  vec3 a=normalize(vec3(-.42,.46,.78));
  vec3 ux=normalize(cross(vec3(0.,0.,1.),a)), uy=cross(a,ux);
  float ca=dot(d,a);
  if(ca>0.){ vec2 q=vec2(dot(d,ux),dot(d,uy))/ca; s+=2.6*smoothstep(.08,-.08,max(abs(q.x)-.55,abs(q.y)-.40))*(.8+.2*smoothstep(-.3,.3,q.y)); }
  vec3 b=normalize(vec3(.82,0.,.57)); float cb=dot(d,b);
  if(cb>0.){ vec3 bx=normalize(cross(vec3(0.,1.,0.),b)); vec2 q=vec2(dot(d,bx),d.y)/cb; s+=1.8*smoothstep(.05,-.05,max(abs(q.x)-.08,abs(q.y)-1.)); }
  s+=.32*smoothstep(-.05,-.7,d.y)*smoothstep(-.4,.2,d.z);
  s+=.25*exp(-pow((d.y+.1)/.03,2.))*smoothstep(-.5,.2,d.z);
  s+=9.*pow(max(dot(d,normalize(vec3(.3,.62,.72))),0.),700.);
  return s;
}
vec3 aces(vec3 x){ return clamp((x*(2.51*x+.03))/(x*(2.43*x+.59)+.14),0.,1.); }
const float TH=.075, RND=.05;
float sdDisc(vec3 p){ vec2 d=vec2(length(p.xy)-(1.-RND), abs(p.z)-(TH-RND)); return min(max(d.x,d.y),0.)+length(max(d,0.))-RND; }
float tex(sampler2D s, vec2 p, float k){ vec2 iu=p/k*.5+.5; vec2 c=clamp(iu,.001,.999); float m=step(0.,iu.x)*step(iu.x,1.)*step(0.,iu.y)*step(iu.y,1.); return texture2D(s,vec2(c.x,1.-c.y)).a*m; }   // sampled in every pixel: no early return (that broke the mip choice at the edge: lines)
void main(){
  vec3 ow, dw; float pxw; cam(ow,dw,pxw,1.12);
  vec3 ro=uRT*ow, rd=uRT*dw;
  float b=dot(ro,rd), c=dot(ro,ro)-1.03, h=b*b-c;
  if(h<0.){ gl_FragColor=vec4(0.); return; }
  float t=-b-sqrt(h), tmax=-b+sqrt(h), md=1e3; bool hit=false;
  for(int i=0;i<72;i++){ float dd=sdDisc(ro+rd*t); md=min(md,dd); if(dd<.0004){ hit=true; break; } t+=dd; if(t>tmax) break; }
  if(!hit){ gl_FragColor=vec4(0.); return; }
  vec3 p=ro+rd*t;
  vec2 e=vec2(.0008,0.);
  vec3 n=normalize(vec3(sdDisc(p+e.xyy)-sdDisc(p-e.xyy),sdDisc(p+e.yxy)-sdDisc(p-e.yxy),sdDisc(p+e.yyx)-sdDisc(p-e.yyx)));
  float tt=uTime;
  bool front=n.z>.6, back=n.z<-.6;
  vec2 q=front?p.xy:vec2(-p.x,p.y);                 // face coordinates as you look at that face
  float r=length(q); vec2 rq=r>1e-4?q/r:vec2(1.,0.);
  float RIM=.83, spin=0., matte=0., rec=0.;
  vec3 nq=vec3(0.,0.,1.);                           // normal in face space (z out of that face)
  float aa=pxw*1.2;
  if(front||back){
    if(r>RIM){
      float x=clamp((r-RIM)/(.95-RIM),0.,1.);
      nq=normalize(vec3(-rq*cos(x*PI)*1.1,1.));    // the raised, rounded bead
    } else {
      nq=normalize(vec3(q*.22,1.));
      float ring=sin(r*900.)*.5+.5;
      nq=normalize(nq+vec3(rq*(ring-.5)*.012,0.));
      spin=1.;
      rec=smoothstep(RIM-.12,RIM,r)*.3;
    }
    // engraving: the icon on the front, the name on the back. Floor = matte (sandblasted), walls = polished.
    float k=front?.66:.92;
    if(r<RIM && ((front && uIconOn>0.) || (back && uBackOn>0.))){
      float dd=1.4*pxw;
      float m=front?tex(uIcon,q,k):tex(uBack,q,k);
      float gx=front?tex(uIcon,q+vec2(dd,0.),k)-tex(uIcon,q-vec2(dd,0.),k):tex(uBack,q+vec2(dd,0.),k)-tex(uBack,q-vec2(dd,0.),k);
      float gy=front?tex(uIcon,q+vec2(0.,dd),k)-tex(uIcon,q-vec2(0.,dd),k):tex(uBack,q+vec2(0.,dd),k)-tex(uBack,q-vec2(0.,dd),k);
      float w=clamp(length(vec2(gx,gy))*1.6,0.,1.);
      nq=normalize(mix(nq,normalize(vec3(vec2(gx,gy)*2.2,1.)),w*.9));
      matte=smoothstep(.15,.85,m)*(1.-w);
      rec+=m;
      if(m>.15) spin=0.;
    }
    rec+=smoothstep(.1,0.,abs(r-RIM))*.8;
  } else {
    // the edge: fine reeding all round
    float ang=atan(p.y,p.x);
    vec3 tg=vec3(-sin(ang),cos(ang),0.);
    n=normalize(n+tg*sin(ang*180.)*.18);
    rec=.15;
  }
  vec3 nl=(front||back)?(front?nq:vec3(-nq.x,nq.y,-nq.z)):n;   // back: undo the mirror and face -z
  vec3 nw=uR*nl;
  float cv=clamp(-dot(nw,dw),0.,1.);
  vec3 F=uMetal+(1.-uMetal)*pow(1.-cv,5.);
  float en=0.;
  if(spin>.5){
    vec3 rw=uR*(front?vec3(rq,0.):vec3(-rq.x,rq.y,0.));
    for(int j=0;j<4;j++){ float o=(float(j)-1.5)/1.5; en+=envS(reflect(dw,normalize(nw+rw*o*.28)),tt); }
    en*=.25*.92;
  } else en=envS(reflect(dw,nw),tt);
  vec3 col=F*en;
  if(matte>0.){ float grain=.85+.3*hash(floor(gl_FragCoord.xy)); col=mix(col,uMetal*(.10+.10*envS(vec3(-.35,.4,.85),tt))*grain,matte); }
  if(front||back){
    col*=1.-.35*smoothstep(RIM-.12,RIM,r)*step(r,RIM);
    col*=1.-smoothstep(aa*1.5,0.,abs(r-RIM))*.7;
  }
  col=aces(col*1.1);
  col=aged(col,1.,(front||back)?q:p.xy,rec,(front||back)?nq.z:mix(.6,1.,smoothstep(0.,.9,p.y/max(length(p.xy),1e-4))),1.,pxw);   // steep walls stay clean; the top of the edge collects dust
  float cover=clamp(-md/pxw+1.,0.,1.);
  gl_FragColor=vec4(col,1.);
}`;

  // ---- coin designs (v6): real relief, not a flat disc. One SDF per design (front), a shared reverse, and Diamond / Ruby
  // set into the Dial design like a lens element in its barrel (closed-back cup behind the stone).
  // uDesign: 0 Dial, 1 Heatsink, 2 Guilloche, 3 Silicon, 4 Turbine, 5 Rack. uStone: 0 none, 1 diamond, 2 ruby.
  const COIN = COMMON + `
uniform vec3 uMetal, uMetal2;   // uDesign and uStone are #defines: one program per design
uniform sampler2D uIcon, uRing, uBack; uniform float uIconOn, uRingOn, uBackOn;
// the studio for metal. Its slow sway (ER) is set once per pixel in main; the light directions are constants (no per-call
// normalize/cross), so the same function is cheap in the app's HLSL port too.
mat2 ER;
const vec3 EA=vec3(-0.420758,0.460830,0.781408), EUX=vec3(-0.738485,-0.674269,0.), EUY=vec3(0.526879,-0.577058,0.624021);
const vec3 EB=vec3(0.821109,0.,0.570771), EBX=vec3(0.570771,0.,-0.821109), ESP=vec3(0.301086,0.622244,0.722606);
float envS(vec3 d, float t){
  d.xy=ER*d.xy;
  float s=.09+.12*smoothstep(-.2,.9,d.y);
  float ca=dot(d,EA);
  if(ca>0.){ vec2 q=vec2(dot(d,EUX),dot(d,EUY))/ca; s+=2.6*smoothstep(.08,-.08,max(abs(q.x)-.55,abs(q.y)-.40))*(.8+.2*smoothstep(-.3,.3,q.y)); }
  float cb=dot(d,EB);
  if(cb>0.){ vec2 q=vec2(dot(d,EBX),d.y)/cb; s+=1.8*smoothstep(.05,-.05,max(abs(q.x)-.08,abs(q.y)-1.)); }
  s+=.32*smoothstep(-.05,-.7,d.y)*smoothstep(-.4,.2,d.z);
  float gy=(d.y+.1)/.03; s+=.25*exp(-gy*gy)*smoothstep(-.5,.2,d.z);
  float sp=max(dot(d,ESP),0.); sp*=sp; sp*=sp; sp*=sp; sp*=sp;   // ^16
  s+=9.*pow(sp,43.75);                                             // ^700
  return s;
}
float envG(vec3 d){
  d.xy=rot(uTime*.05)*d.xy;
  float az=atan(d.y,d.x);
  float s=.03+.45*smoothstep(-.1,.9,d.z);
  s+=1.1*pow(max(cos(az*3.)*.5+.5,0.),5.)*smoothstep(-.2,.5,d.z)*smoothstep(.98,.75,d.z);
  s+=pow(max(dot(d,normalize(vec3(.55,.55,.63))),0.),140.)*8.;
  s+=pow(max(dot(d,normalize(vec3(-.72,.25,.64))),0.),100.)*5.;
  s+=pow(max(dot(d,normalize(vec3(.05,-.78,.62))),0.),180.)*7.;
  s+=1.5*smoothstep(.72,.9,d.z)*smoothstep(.99,.95,d.z)*(.75+.25*cos(az*8.));
  s*=1.-.9*smoothstep(.975,.995,d.z);
  return s;
}
vec3 aces(vec3 x){ return clamp((x*(2.51*x+.03))/(x*(2.43*x+.59)+.14),0.,1.); }
float tex(sampler2D s, vec2 p, float k){ vec2 iu=p/k*.5+.5; vec2 c=clamp(iu,.001,.999); float m=step(0.,iu.x)*step(iu.x,1.)*step(0.,iu.y)*step(iu.y,1.); return texture2D(s,vec2(c.x,1.-c.y)).a*m; }
${geo()}
const float SS=.57;                                  // set stone: girdle radius
// Dial family (design 6): the stone sits forward in a raised collet: girdle level with the grip ring, the whole crown proud;
// the pavilion rests in a fitted polished cup, the back stays flat (no bulge)
const float SSD=uDesign>5.5?.55:SS;
const float ZG6=.19;                                          // girdle height
const float GZD=uDesign>5.5?(ZG6-.2795*SSD):(.15-TABC*SS);    // stone centre (z) / old Dial: table at .15
const float FLOOR6=-.13;                                      // the culet stops just inside the back plate
// Emulsion (stone 3): a soap-film dome in the same collet, over a dark well with the mark floating in it
const float ZF=0., RF=.685;                                   // Emulsion (stone 3): the bubble (its own shader) in an open ring, just inside the bezel
const float SCUT=-(FLOOR6-GZD)/SSD;
#define P(N,C) { vec3 nn=N; float dn=dot(nn,d); float tt=(C-dot(nn,o))/dn; if(dn<0.){ if(tt>tIn){ tIn=tt; nIn=nn; } } else { if(tt<tOut){ tOut=tt; nOut=nn; } } }
void trav(vec3 o, vec3 d, out float tIn, out vec3 nIn, out float tOut, out vec3 nOut){
  tIn=-1e9; tOut=1e9; nIn=vec3(0.,0.,1.); nOut=vec3(0.,0.,1.);
  P(vec3(0.,0.,1.),TABC)
  if(uDesign>5.5) P(vec3(0.,0.,-1.),SCUT)          // Dial family: the pavilion ends flat on the closed back plate (no bulge)
  vec2 cs=vec2(1.,0.);
  for(int k=0;k<16;k++){
    vec2 ch=vec2(cs.x*HALF.x-cs.y*HALF.y, cs.y*HALF.x+cs.x*HALF.y);
    P(vec3(cs,0.),1.)
    P(vec3(ch*UGI.x,UGI.y),UGIC)
    P(vec3(ch*LGI.x,LGI.y),LGIC)
    if(k==2*(k/2)){ P(vec3(cs*BEZ.x,BEZ.y),BEZC) P(vec3(cs*PAV.x,PAV.y),PAVC) }
    else { P(vec3(cs*STA.x,STA.y),STAC) }
    cs=vec2(cs.x*ROT8.x-cs.y*ROT8.y, cs.y*ROT8.x+cs.x*ROT8.y);
  }
}
float fres(float c, float ior){ float F0=pow((ior-1.)/(ior+1.),2.); return F0+(1.-F0)*pow(1.-clamp(c,0.,1.),5.); }

// ---- shapes
float box2(vec2 p, vec2 h, float rr){ vec2 d=abs(p)-h+rr; return length(max(d,0.))+min(max(d.x,d.y),0.)-rr; }
float box3(vec3 p, vec3 h, float rr){ vec3 q=abs(p)-h+rr; return length(max(q,0.))+min(max(q.x,max(q.y,q.z)),0.)-rr; }
float ringS(vec3 p, float R0, float R1, float z0, float z1, float rr){ return box2(vec2(length(p.xy),p.z)-vec2((R0+R1)*.5,(z0+z1)*.5), vec2((R1-R0)*.5,(z1-z0)*.5), rr); }
float disc(vec3 p, float R, float z0, float z1, float rr){ return box2(vec2(length(p.xy),p.z-(z0+z1)*.5), vec2(R,(z1-z0)*.5), rr); }
vec2 prep(vec2 q, float n){ float an=TAU/n; float a=mod(atan(q.y,q.x)+an*.5,an)-an*.5; return length(q)*vec2(cos(a),sin(a)); }
vec2 U(vec2 a, vec2 b){ return b.x<a.x?b:a; }
float Hb(float r){ return .12+.45*(1.-r*r/.41); }   // the closed cup behind a set stone (a lens's rear dome)
// ids: 1 polished, 2 spun (turned), 3 black anodised, 4 sandblasted, 6 second metal polished, 7 silicon die, 8 guilloche, 9 screw
vec2 mapF(vec3 p){
  float r=length(p.xy);
  vec2 d=vec2(1e3,0.);
  if(uDesign<.5){                                    // Dial: grip ring with beads, a sloped bezel with the scale, black barrel
    d=U(d,vec2(ringS(p,.9,1.,-.01,.19,.025),1.));
    vec2 bq=prep(p.xy,72.); d=U(d,vec2(length(vec3(bq.x-.948,bq.y,p.z-.186))-.016,1.));
    d=U(d,vec2(max(ringS(p,.70,.905,-.01,.4,.0),(p.z-(.15+(r-.70)*.13))*.99),2.));
    d=U(d,vec2(ringS(p,.60,.705,-.01,.08,.012),3.));
    if(uStone<.5) d=U(d,vec2(max(r-.62,max(-.01-p.z,p.z-(.05+.04*(1.-r*r/.384))))*.95,2.));
  } else if(uDesign<1.5){                            // Heatsink: copper slug, 40 aluminium fins, rim lip
    d=U(d,vec2(disc(p,.99,-.01,.045,.02),4.));
    d=U(d,vec2(ringS(p,.945,1.,-.01,.11,.02),1.));
    vec2 fq=prep(p.xy,40.);
    float top=.155-.06*clamp((r-.46)/.48,0.,1.);
    d=U(d,vec2(max(box2(fq-vec2(.70,0.),vec2(.245,.011),.005),max(p.z-top,-.01-p.z))*.9,1.));
    d=U(d,vec2(disc(p,.40,-.01,.15,.035),6.));
  } else if(uDesign<2.5){                            // Guilloche: stepped rim, engine-turned field, beaded cartouche
    d=U(d,vec2(ringS(p,.92,1.,-.01,.13,.025),1.));
    d=U(d,vec2(ringS(p,.85,.925,-.01,.11,.012),1.));
    d=U(d,vec2(disc(p,.855,-.01,.075,.005),8.));
    d=U(d,vec2(disc(p,.39,-.01,.11,.035),1.));
    vec2 bq=prep(p.xy,48.); d=U(d,vec2(length(vec3(bq.x-.435,bq.y,p.z-.075))-.014,1.));
  } else if(uDesign<3.5){                            // Silicon: a CPU die with gold pins on a blasted field, traces to the rim
    d=U(d,vec2(ringS(p,.9,1.,-.01,.12,.025),1.));
    d=U(d,vec2(disc(p,.905,-.01,.07,.005),4.));
    d=U(d,vec2(box3(p-vec3(0.,0.,.06),vec3(.27,.27,.07),.025),7.));
    vec2 a2=abs(p.xy); if(a2.y>a2.x) a2=a2.yx;
    float yy=a2.y-.07*clamp(floor(a2.y/.07+.5),0.,3.);
    d=U(d,vec2(box3(vec3(a2.x-.31,yy,p.z-.075),vec3(.04,.014,.012),.006),6.));
  } else if(uDesign<4.5){                            // Turbine: nine curved blades turning slowly in a black well
    d=U(d,vec2(ringS(p,.84,1.,-.01,.13,.025),1.));
    d=U(d,vec2(disc(p,.845,-.01,.02,.005),3.));
    float an=TAU/9., a=mod(atan(p.y,p.x)-1.25*r+uTime*.35+an*.5,an)-an*.5;
    float bl=max(max(abs(r*sin(a))-.02,max(r-.815,.2-r)),max(p.z-(.11-.045*(r-.2)),-.01-p.z));
    d=U(d,vec2(bl*.55,1.));
    d=U(d,vec2(disc(p,.23,-.01,.155,.045),2.));
  } else if(uDesign<5.5){                            // Rack: terraces like a 1U faceplate, four rack screws
    d=U(d,vec2(ringS(p,.88,1.,-.01,.15,.02),1.));
    d=U(d,vec2(ringS(p,.755,.885,-.01,.12,.012),2.));
    d=U(d,vec2(ringS(p,.635,.76,-.01,.09,.012),1.));
    d=U(d,vec2(disc(p,.64,-.01,.065,.01),2.));
    vec2 sq=prep(rot(PI/4.)*p.xy,4.);
    d=U(d,vec2((length(vec3(sq.x-.82,sq.y,(p.z-.12)*1.7))-.05)/1.7,9.));
  } else {                                           // Dial family: the Dial frame; metals hold a CPU die, Diamond and Ruby a stone
    // (cheap where it can be: the 72 beads are only looked up near the grip ring, the centre only inside the barrel;
    //  elsewhere a bound that is never larger than the true distance stands in)
    d=U(d,vec2(ringS(p,.9,1.,-.01,.19,.025),1.));
    if(r>.9){ vec2 bq=prep(p.xy,72.); d=U(d,vec2(length(vec3(bq.x-.948,bq.y,p.z-.186))-.016,1.)); }
    else d.x=min(d.x,length(vec2(.932-r,max(abs(p.z-.186)-.016,0.))));
    d=U(d,vec2(max(ringS(p,.70,.905,-.01,.4,.0),(p.z-(.15+(r-.70)*.13))*.99),2.));
    if(uStone>2.5) return d;                         // Emulsion: open ring, nothing behind the bubble
    d=U(d,vec2(ringS(p,.595,.705,-.01,.08,.012),3.));
    if(r>.63) d.x=min(d.x,r-.6);
    else if(uStone<.5){
      d=U(d,vec2(disc(p,.6,-.01,.05,.005),4.));
      d=U(d,vec2(box3(p-vec3(0.,0.,.06),vec3(.235,.235,.07),.022),7.));
      vec2 a2=abs(p.xy); if(a2.y>a2.x) a2=a2.yx;
      float yy=a2.y-.06*clamp(floor(a2.y/.06+.5),0.,3.);
      d=U(d,vec2(box3(vec3(a2.x-.27,yy,p.z-.065),vec3(.035,.012,.011),.005),6.));
    } else {                                         // the collet: a polished cup rising out of the barrel, a thin lip at the girdle
      float body=box2(vec2(r-.3,p.z-(ZG6+.012-.12)*.5),vec2(.3,(ZG6+.012+.12)*.5),.01);
      float k=.602, zc=GZD-.602*SSD-.004, pocket=max(r-(SSD+.003),(zc+k*r-p.z)/sqrt(1.+k*k));
      d=U(d,vec2(max(body,-pocket),6.));
    }
  }
  return d;
}
vec2 mapB(vec3 p){                                   // the reverse: a rim and a turned field with the name
  if(p.z>.06) return vec2(p.z-.012,1.);              // everything here is below z=.01
  float r=length(p.xy), dep=(uDesign<.5||uDesign>5.5)?.19:.12;
  vec2 d=vec2(ringS(p,.9,1.,-dep,.01,.025),1.);
  d=U(d,vec2(disc(p,.905,-dep+.05,.01,.01),2.));
  if(uStone>2.5) d.x=max(d.x,.705-r);                // Emulsion: the ring is open right through
  if(uStone>.5 && uDesign<.5) d=U(d,vec2(max(r-.64,max(-p.z-Hb(r),p.z-.01))/1.7,1.));
  return d;
}
vec2 map(vec3 p){
  if(p.z<-.06 && !(uStone>.5 && uDesign<.5)) return mapB(p);   // the front is all above z=-.01 (the Dial's cup is the exception)
  vec2 d=U(mapF(p),mapB(p));
  if(uStone>.5 && uDesign<.5){ float r=length(p.xy); float h=max(r-.60,(-Hb(r)+.03)-p.z)/1.7; if(-h>d.x) d=vec2(-h,6.); }
  return d;
}
vec3 nrm(vec3 p){ const vec2 k=vec2(1.,-1.); const float e=.0012*.5773; return normalize(k.xyy*map(p+k.xyy*e).x+k.yyx*map(p+k.yyx*e).x+k.yxy*map(p+k.yxy*e).x+k.xxx*map(p+k.xxx*e).x); }

// the set stone, shaded like the full-view gem; light that leaves through the pavilion meets the polished cup
vec3 stone(vec3 o, vec3 d, float tIn, vec3 nIn, vec3 M, out float cover, float pxw, float tOut){
  bool ruby=uStone>1.5;
  float ior=ruby?1.77:2.417;
  vec3 iorC=ruby?vec3(1.762,1.77,1.778):vec3(2.37,2.417,2.47);
  vec3 absorb=ruby?vec3(.12,3.4,2.7):vec3(.004,.006,.01);
  cover=clamp((tOut-tIn)/(pxw*2.5),0.,1.);
  vec3 p=o+d*tIn;
  float F=fres(-dot(d,nIn),ior);
  vec3 col=vec3(F*envG(uR*reflect(d,nIn)))*.8;
  vec3 di=refract(d,nIn,1./ior);
  vec3 thr=vec3(1.-F);
  for(int b=0;b<6;b++){
    float a1,a2; vec3 n1,n2;
    trav(p+di*1e-4,di,a1,n1,a2,n2);
    float L=max(a2,0.);
    thr*=exp(-absorb*L);
    p=p+di*L;
    float c=dot(di,n2);
    vec3 outM=refract(di,-n2,ior);
    if(dot(outM,outM)<1e-4){ di=reflect(di,n2); continue; }
    float Fo=fres(c,ior);
    vec3 e;
    if(n2.z<-.2){
      // into the cup: polished metal sends it back up, a little warmer/cooler with the metal
      vec3 up=normalize(vec3(outM.xy,abs(outM.z)));
      e=M*(.18+.8*envS(uR*up,uTime))*vec3(.95);
    } else {
      vec3 oR=refract(di,-n2,iorC.r), oG=refract(di,-n2,iorC.g), oB=refract(di,-n2,iorC.b);
      e.r=dot(oR,oR)>1e-4?envG(uR*oR):0.; e.g=envG(uR*oG); e.b=dot(oB,oB)>1e-4?envG(uR*oB):0.;
    }
    col+=thr*(1.-Fo)*e;
    thr*=Fo;
    di=reflect(di,n2);
  }
  col+=thr*.04;
  if(ruby){
    col*=vec3(1.,.55,.6);
    col+=vec3(.16,.005,.025)+vec3(.5,.02,.06)*pow(.5+.5*sin(uTime*.8+dot(nIn,vec3(3.1,1.7,2.3))*4.),3.)*.35;
    col=pow(col,vec3(1.15));
  } else col=pow(col*.8,vec3(1.7))*1.25+.01;
  return 1.-exp(-max(col,0.)*1.6);
}

// Emulsion: the real bubble is drawn first (js: bubblePass); the coin stays clear wherever the bubble is in front of the metal
vec4 bubble(vec3 ro, vec3 rd, float tHit, vec4 base){
  vec3 oc=ro-vec3(0.,0.,ZF); float bb=dot(oc,rd), hh=bb*bb-dot(oc,oc)+RF*RF;
  if(hh>0. && -bb-sqrt(hh)<tHit) return vec4(0.);
  return base;
}

void main(){
  vec3 ow, dw; float pxw; cam(ow,dw,pxw,1.24);
  ER=rot(.12*sin(uTime*.21));
  vec3 ro=uRT*ow, rd=uRT*dw;
  float b=dot(ro,rd), c=dot(ro,ro)-1.44, h=b*b-c;
  if(h<0.){ gl_FragColor=vec4(0.); return; }
  float t=max(-b-sqrt(h),0.), tmax=-b+sqrt(h); bool hit=false;
  if(uDesign>5.5){                                   // the metal fits in r<1, -.2<z<.215: clip the march to that cylinder
    float a2=dot(rd.xy,rd.xy), b2=dot(ro.xy,rd.xy), c2=dot(ro.xy,ro.xy)-1.;
    if(a2>1e-6){ float h2=b2*b2-a2*c2; if(h2<0.) tmax=-1.; else { h2=sqrt(h2); t=max(t,(-b2-h2)/a2); tmax=min(tmax,(-b2+h2)/a2); } }
    if(abs(rd.z)>1e-6){ float z0=(-.2-ro.z)/rd.z, z1=(.215-ro.z)/rd.z; t=max(t,min(z0,z1)); tmax=min(tmax,max(z0,z1)); }
  }
  float eps=max(.0005,pxw*.3); for(int i=0;i<140;i++){ if(t>tmax) break; float dd=map(ro+rd*t).x; if(dd<eps){ hit=true; break; } t+=dd*.9; }
  vec3 M=uMetal;
  // the set stone in front of the metal?
  float scov=0.; vec3 sc=vec3(0.);
  if(uStone>.5 && uStone<2.5){
    vec3 og=(ro-vec3(0.,0.,GZD))/SSD;
    float gIn,gOut; vec3 gnIn,gnOut;
    float sb=dot(og,rd), sc2=dot(og,og)-1.1025;
    gIn=1.; gOut=0.;
    if(sb*sb-sc2>0.) trav(og,rd,gIn,gnIn,gOut,gnOut);
    if(gIn<gOut && gOut>0. && (!hit || gIn*SSD<t)){
      sc=stone(og,rd,gIn,gnIn,M,scov,pxw/SSD,gOut);
      if(scov>.999 || !hit){ gl_FragColor=vec4(sc*scov,scov); return; }
    }
  }
  if(!hit){ gl_FragColor=vec4(0.); return; }
  vec3 p=ro+rd*t;
  vec2 mm=map(p); float id=mm.y;
  vec3 n=nrm(p);
  float tt=uTime;
  bool fr=p.z>0.;
  vec2 q=fr?p.xy:vec2(-p.x,p.y);
  float r=length(q); vec2 rq=r>1e-4?q/r:vec2(1.,0.);
  float aa=pxw*1.2;
  if(id>5.5&&id<6.5) M=uMetal2;
  vec3 nl=n;
  float matte=0., paint=0.;
  bool face=abs(n.z)>.6;
  // the outer wall: fine reeding
  if(length(p.xy)>.975 && abs(n.z)<.6){ float ang=atan(p.y,p.x); vec3 tg=vec3(-sin(ang),cos(ang),0.); nl=normalize(nl+tg*sin(ang*220.)*.22); }
  // engine turning: a waved rosette cut into the field
  if(id>7.5&&id<8.5){
    float ang=atan(q.y,q.x);
    float g=r*150.+2.2*sin(ang*16.)+1.2*sin(ang*5.-r*9.);
    vec2 gr=rq*150.*cos(g);
    nl=normalize(nl+vec3(fr?gr:vec2(-gr.x,gr.y),0.)*.0016*smoothstep(.4,.46,r));
  }
  // the icon, engraved (matte floor, polished walls) where each design keeps it
  float kI=uDesign>5.5?.17:uDesign<.5?.42:uDesign<1.5?.27:uDesign<2.5?.27:uDesign<3.5?.2:uDesign<4.5?.15:.4;
  bool onDie=id>6.5&&id<7.5&&n.z>.7;
  if(fr && face && uIconOn>0. && r<kI*1.1 && uStone<.5){
    float dd=1.4*pxw;
    float m=tex(uIcon,q,kI);
    float gx=tex(uIcon,q+vec2(dd,0.),kI)-tex(uIcon,q-vec2(dd,0.),kI), gy=tex(uIcon,q+vec2(0.,dd),kI)-tex(uIcon,q-vec2(0.,dd),kI);
    float w=clamp(length(vec2(gx,gy))*1.6,0.,1.);
    if(onDie){ paint=-smoothstep(.2,.8,m); }                     // the mark on the die is bright metal (etched)
    else { nl=normalize(nl+vec3(gx,gy,0.)*2.2*w); matte=smoothstep(.15,.85,m)*(1.-w); }
  }
  // the name on the reverse
  if(!fr && face && uBackOn>0. && r<.9){
    float dd=1.4*pxw;
    float m=tex(uBack,q,.92);
    float gx=tex(uBack,q+vec2(dd,0.),.92)-tex(uBack,q-vec2(dd,0.),.92), gy=tex(uBack,q+vec2(0.,dd),.92)-tex(uBack,q-vec2(0.,dd),.92);
    float w=clamp(length(vec2(gx,gy))*1.6,0.,1.);
    nl=normalize(nl+vec3(-gx,gy,0.)*2.2*w); matte=max(matte,smoothstep(.15,.85,m)*(1.-w));
  }
  // paint-filled engraving on the front: the ring text, the dial's scale, the die's traces
  if(fr && n.z>.3){
    if(uRingOn>0.) paint=max(paint,smoothstep(.1,.7,tex(uRing,q,1.)));
    if((uDesign<.5||uDesign>5.5) && id>1.5&&id<2.5 && r>.71){
      float u=atan(q.y,q.x)/TAU*60., fi=floor(u+.5), du=abs(u-fi)*TAU/60.*r; bool maj=mod(fi+60.,5.)<.5;
      float w=maj?.0065:.0035, r0=maj?.80:.84;
      paint=max(paint,smoothstep(w+aa,w,du)*smoothstep(r0-aa,r0,r)*smoothstep(.888,.884,r)*step(-.35,q.y/max(r,1e-3)));
    }
  }
  float trace=0.;
  if((uDesign>2.5&&uDesign<3.5 || uDesign>5.5)&&fr&&id>3.5&&id<4.5){
    bool fam=uDesign>5.5;                            // Dial family: shorter traces, pads just inside the barrel
    float r0=fam?.3:.36, r1=fam?.54:.84, j0=fam?.4:.56, j1=fam?.46:.66, tw=fam?.0045:.006, pr=fam?.016:.022;
    float an=TAU/24., a=atan(q.y,q.x), k=floor((a+an*.5)/an), la=a-k*an;
    float jog=smoothstep(j0,j1,r)*an*.35*(mod(k,2.)<.5?1.:-1.);
    float dl=abs(r*sin(la-jog))-tw;
    float tr=smoothstep(aa,-aa,dl)*step(r0,r)*step(r,r1);
    float pad=smoothstep(pr+aa,pr,length(q-r1*vec2(cos(k*an+jog),sin(k*an+jog))));
    trace=max(tr,pad);
  }
  vec3 nw=uR*nl;
  float cv=clamp(-dot(nw,dw),0.,1.);
  vec3 R=reflect(dw,nw);
  if(uStone>2.5){ float ph=1.3*cv+atan(q.y,q.x)/TAU*2.+r*1.2+.15*sin(uTime*.2); M=mix(M,(.62+.38*cos(TAU*(ph+vec3(0.,.33,.67))))*M,.42); }
  vec3 F=M+(1.-M)*pow(1.-cv,5.);
  float fz=pow(1.-cv,5.);
  vec3 col;
  if(id>1.5&&id<2.5){                                // turned: light smeared round the centre
    vec3 rw=uR*(fr?vec3(rq,0.):vec3(-rq.x,rq.y,0.));
    float en=0.; for(int j=0;j<4;j++){ float o=(float(j)-1.5)/1.5; en+=envS(reflect(dw,normalize(nw+rw*o*.28)),tt); }
    col=F*en*.25*.92;
  } else if(id>2.5&&id<3.5){                         // black anodised
    col=vec3(.012)+(.04+.96*fz)*envS(R,tt)*.5;
  } else if(id>3.5&&id<4.5){                         // sandblasted
    col=M*(.07+.12*envS(nw,tt))*(.85+.3*hash(floor(gl_FragCoord.xy)))+F*envS(R,tt)*.05;
  } else if(id>6.5&&id<7.5&&n.z>.7){                 // the die: dark silicon, a glassy sheen
    col=vec3(.02,.024,.034)+(.05+.95*fz)*envS(R,tt)*.45;
  } else col=F*envS(R,tt);                           // polished
  if(id>8.5){                                        // screw: a cross slot
    vec2 sq=prep(rot(PI/4.)*p.xy,4.)-vec2(.82,0.);
    vec2 sl=rot(.5)*sq;
    float slot=max(smoothstep(.007+aa,.007,abs(sl.x)),smoothstep(.007+aa,.007,abs(sl.y)))*step(length(sq),.036);
    col*=1.-.9*slot;
  }
  if(trace>0.){ vec3 G=uMetal2+(1.-uMetal2)*fz; col=mix(col,G*envS(R,tt),trace); }
  if(matte>0.){ float grain=.85+.3*hash(floor(gl_FragCoord.xy)); col=mix(col,M*(.10+.10*envS(vec3(-.35,.4,.85),tt))*grain,matte); }
  if(paint>0.) col=mix(col,vec3(.018),paint*.94);
  if(paint<0.){ vec3 G=M+(1.-M)*fz; col=mix(col,G*envS(R,tt),-paint); }
  col=aces(col*1.1);
  gl_FragColor=vec4(mix(col,sc,scov),1.);
  if(uStone>2.5) gl_FragColor=bubble(ro,rd,t,gl_FragColor);
}`;

  const LOOK3 = { lqRefr: 'uLqRefr', lqRefl: 'uLqRefl', lqFilm: 'uLqFilm', lqTint: 'uLqTint', lqWobble: 'uLqWob', lqDrops: 'uLqDrops', lqDropSize: 'uLqDropS', lqMove: 'uLqMove', dotRefr: 'uDotRefr', dotRing: 'uDotRing', refrPeriod: 'uRefrT', edgeWidth: 'uEdgeW', edgeVary: 'uEdgeVary', edgeColour: 'uEdgeCol', rimLine: 'uRim', saturation: 'uSat', filmInside: 'uFilmIn', filmThick: 'uThick', flowSpeed: 'uFlow', reflection: 'uRefl', glow: 'uGlow', breath: 'uBreath', motionAmt: 'uMotAmt', motionTime: 'uMotT' };
  let gen = 0, cv = null, gl = null, cur = null, pending = null, raf = 0, t0 = 0, kind = 'dia', R = [1, 0, 0, 0, 1, 0, 0, 0, 1], dirty = true, still = false, wear = [0, 0], opt = {}, texI = null, texB = null, texR = null, vis = true, io = null;
  const SRC = () => ({ bubble: BUBBLE, medal: MEDAL, gem: GEM, coin: COIN });
  const NAMES = ['uRes', 'uTime', 'uR', 'uRT', 'uKind', 'uWear', 'uAge', 'uMetal', 'uIcon', 'uIconOn', 'uBack', 'uBackOn', 'uMetal2', 'uRing', 'uRingOn'].concat(Object.values(LOOK3));
  /** Which program a view needs: one per kind, aged or clean for metal and stones, one per coin design (#defines). */
  function variant(k, o, w) {
    const name = k === 'bubble' ? 'bubble' : k === 'medal' ? 'medal' : k === 'coin' ? 'coin' : 'gem';
    let defs = '';
    if (name === 'coin') defs = `#define uDesign ${(o && o.design) || 0}.\n#define uStone ${(o && o.stone) || 0}.\n`;
    else if ((name === 'medal' || name === 'gem') && w && (w[0] > 0 || (w[2] || 0) > 0)) defs = '#define AGED\n';
    return { key: '3d:' + name + (defs ? ':' + defs.replace(/#define |\n/g, ' ').trim() : ''), name, defs };
  }
  /* The Emulsion coin (stone 3): the real bubble shader (same look, noise and tuning as the badge and the full view), scaled so its
     sphere is the coin's ring sphere (radius .685; 1.0142*1.24/.6895 matches the outlines under the same eye), drawn first; the
     coin is drawn over it and stays clear wherever the bubble is in front. */
  const IN_RING = { key: '3d:bubble:in-ring', name: 'bubble', defs: '#define BEXT 1.8239\n' };
  const inRing = (o) => !!o && o.stone === 3;
  function bubblePass(M, w, h, t) {
    if (!root.YTPMLook) return false;                 // its look (js/bubble.js) is not loaded yet
    const b = prog(IN_RING, () => { dirty = true; });
    if (!b || !b.ok) return false;
    root.YTPMGL.use(b);
    const U = b.U, look = root.YTPMLook || {};
    gl.uniform2f(U.uRes, w, h); gl.uniform1f(U.uTime, t); gl.uniform1f(U.uKind, 0);
    gl.uniformMatrix3fv(U.uR, false, [M[0], M[3], M[6], M[1], M[4], M[7], M[2], M[5], M[8]]); gl.uniformMatrix3fv(U.uRT, false, M);
    for (const k in LOOK3) if (U[LOOK3[k]]) gl.uniform1f(U[LOOK3[k]], +(look[k] != null ? look[k] : 0));
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    return true;
  }
  /** The program for a variant: compiled in the background (shared context, parallel compile), usable once x.ok. */
  const prog = (v, cb) => root.YTPMGL.program(v.key, () => SRC()[v.name], v.defs, NAMES, cb);
  /** Switch the view to a variant: right away when it is compiled, else when it is ready (the old one stays until then). */
  function want(v) {
    const x = prog(v, (y) => { if (y.ok && pending === y) { cur = y; pending = null; dirty = true; } });
    if (!x) return;
    if (x.ok) { cur = x; pending = null; dirty = true; } else pending = x;
  }
  /** Warm up a few seconds after the page has loaded: the full view's programs compile in the background so it opens at once. */
  function warm() {
    const G = root.YTPMGL;
    if (!G || !G.ctx()) return;
    const list = [variant('medal', null, [1, 0, 0]), variant('dia', null, [1, 0, 0]), variant('bubble')];   // clean variants compile when first needed
    let i = 0;   // one at a time, when idle: a program a view needs right now never waits behind the warm-up
    const step = () => { if (i < list.length) G.when(prog(list[i++])).then(() => G.idle(step, 1500)); };
    G.idle(step, 1500);
  }
  function frame(now) {
    raf = 0;
    if (!cv) return;
    const G = root.YTPMGL;
    if (!document.hidden && vis && (dirty || !still) && cur && cur.ok && gl && !gl.isContextLost()) {
      G.left(now);   // counts toward the shared per-frame budget
      const t = still ? 9 : (now - t0) / (kind === 'bubble' ? 1000 : 3000) + (kind === 'coin' ? 9 : 0);   // stones move at a third of real time
      G.size(cv.width, cv.height);
      gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
      const under = kind === 'coin' && inRing(opt) && bubblePass(R, cv.width, cv.height, still ? 14 : (now - t0) / 1000);
      G.use(cur);
      const U = cur.U;
      gl.uniform2f(U.uRes, cv.width, cv.height); gl.uniform1f(U.uTime, t); gl.uniform1f(U.uKind, kind === 'ruby' ? 1 : 0);
      // column-major for GL: R maps object -> world
      gl.uniformMatrix3fv(U.uR, false, [R[0], R[3], R[6], R[1], R[4], R[7], R[2], R[5], R[8]]);
      gl.uniformMatrix3fv(U.uRT, false, R);
      const look = root.YTPMLook || {};
      for (const k in LOOK3) if (U[LOOK3[k]]) gl.uniform1f(U[LOOK3[k]], +(look[k] != null ? look[k] : 0));
      if (U.uWear) gl.uniform3f(U.uWear, wear[0], wear[1], wear[2] || 0);
      if (U.uAge && root.YTPMAgeVec) gl.uniform4fv(U.uAge, root.YTPMAgeVec());
      if (kind === 'coin') coinUniforms(cur, opt, { I: texI, B: texB, R: texR });
      if (kind === 'medal') {
        gl.uniform3fv(U.uMetal, opt.metal || [1, .77, .33]);
        gl.uniform1i(U.uIcon, 0); gl.uniform1i(U.uBack, 1);
        gl.uniform1f(U.uIconOn, texI ? 1 : 0); gl.uniform1f(U.uBackOn, texB ? 1 : 0);
        gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, texB); gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, texI);
      }
      if (under) { gl.enable(gl.BLEND); gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA); }
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      if (under) gl.disable(gl.BLEND);
      G.out(cv);   // onto the visible canvas
      dirty = false;
    }
    raf = requestAnimationFrame(frame);
  }
  /** Start on `canvas` (sized by the caller: width/height in device px). Returns false without WebGL or if the shader fails. */
  /** A 512 px alpha texture from a canvas, mipmapped (power of two). */
  function upload(c) {
    const tx = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tx);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, c);
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return tx;
  }
  /** The medal's engravings: the icon (front) and the name round the back. Drawn white on clear, edges softened like the flat medal's. */
  function medalTextures(o, g) {
    const mk = () => { const c = document.createElement('canvas'); c.width = c.height = 512; return c; };
    const my = gen;   // a later start() (another medal) must not get this medal's icon
    const b = mk(), x = b.getContext('2d');
    x.fillStyle = '#fff'; x.textAlign = 'center'; x.textBaseline = 'middle';
    const font = '"Chakra Petch", "Segoe UI", system-ui, sans-serif';
    x.font = `700 ${o.title && o.title.length > 14 ? 40 : 50}px ${font}`; x.fillText(o.title || '', 256, 236);
    x.font = `600 26px ${font}`; x.fillText((o.sub || '').toUpperCase(), 256, 296);
    const ring = 'RESOURCE MONITOR  ·  UPTIME MEDAL  ·  ', R0 = 196;
    x.font = `600 24px ${font}`;
    const tot = x.measureText(ring + ring).width, step = Math.PI * 2 / tot;
    let a = -Math.PI / 2 - (x.measureText(ring).width * step) / 2;
    for (const ch of ring + ring) { const w = x.measureText(ch).width; x.save(); x.translate(256 + Math.cos(a + w * step / 2) * R0, 256 + Math.sin(a + w * step / 2) * R0); x.rotate(a + w * step / 2 + Math.PI / 2); x.fillText(ch, 0, 0); x.restore(); a += w * step; }
    x.lineWidth = 4; x.strokeStyle = '#fff'; x.beginPath(); x.arc(256, 256, 168, 0, Math.PI * 2); x.stroke();
    if (g === gl && gl && my === gen) { texB = upload(b); dirty = true; }
    const src = root.YTPMIconSrc && o.icon ? root.YTPMIconSrc(o.icon) : '';
    if (!src) return;
    const img = new Image();
    img.onload = () => {
      if (g !== gl || !gl || my !== gen) return;
      const c = mk(), y = c.getContext('2d');
      if ('filter' in y) y.filter = 'blur(1.4px)';
      y.drawImage(img, 0, 0, 512, 512);
      texI = upload(c); dirty = true;
    };
    img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="512" height="512" fill="none" stroke="#fff" color="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${src}</svg>`);
  }
  /** Coin textures: icon (front, engraved), name (reverse), ring text (front, paint-filled). Resolves when the icon is drawn. */
  function coinTex(o) {
    const g = gl;
    const mk = () => { const c = document.createElement('canvas'); c.width = c.height = 1024; return c; };
    const font = '"Chakra Petch", "Segoe UI", system-ui, sans-serif';
    const arc = (x, text, rad, px, bottom) => {   // text along a circle (radius in coin units: 1 = rim), upright to the centre
      x.font = `600 ${px}px ${font}`;
      const R0 = rad * 512, tw = x.measureText(text).width, span = tw / R0;
      let a = bottom ? Math.PI / 2 + span / 2 : -Math.PI / 2 - span / 2;
      for (const ch of text) { const w = x.measureText(ch).width, mid = a + (bottom ? -1 : 1) * w / R0 / 2; x.save(); x.translate(512 + Math.cos(mid) * R0, 512 + Math.sin(mid) * R0); x.rotate(bottom ? mid - Math.PI / 2 : mid + Math.PI / 2); x.fillText(ch, 0, 0); x.restore(); a += (bottom ? -1 : 1) * w / R0; }
    };
    const out = { I: null, B: null, R: null };
    if (!g) return Promise.resolve(out);
    const b = mk(), x = b.getContext('2d');
    x.fillStyle = '#fff'; x.textAlign = 'center'; x.textBaseline = 'middle';
    x.font = `700 ${o.title && o.title.length > 14 ? 80 : 100}px ${font}`; x.fillText(o.title || '', 512, 472);
    x.font = `600 52px ${font}`; x.fillText((o.sub || '').toUpperCase(), 512, 592);
    arc(x, 'RESOURCE MONITOR  ·  UPTIME MEDAL', .82, 46, false);
    arc(x, 'EMULSION LABS', .82, 40, true);
    x.lineWidth = 6; x.strokeStyle = '#fff'; x.beginPath(); x.arc(512, 512, 330, 0, Math.PI * 2); x.stroke();
    out.B = upload(b);
    if (o.ring && o.ring.length) {
      const c = mk(), y = c.getContext('2d'); y.fillStyle = '#fff'; y.textAlign = 'center'; y.textBaseline = 'middle';
      for (const t of o.ring) arc(y, t.text, t.r, t.px, !!t.bottom);
      out.R = upload(c);
    }
    const src = root.YTPMIconSrc && o.icon ? root.YTPMIconSrc(o.icon) : '';
    if (!src) return Promise.resolve(out);
    return new Promise((res) => {
      const img = new Image();
      img.onload = () => {
        if (g === gl) { const c = document.createElement('canvas'); c.width = c.height = 512; const y = c.getContext('2d'); if ('filter' in y) y.filter = 'blur(1.4px)'; y.drawImage(img, 0, 0, 512, 512); out.I = upload(c); }
        res(out);
      };
      img.onerror = () => res(out);
      img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="512" height="512" fill="none" stroke="#fff" color="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${src}</svg>`);
    });
  }
  function coinUniforms(x, o, tx) {
    gl.uniform3fv(x.U.uMetal, o.metal || [1, .77, .33]); gl.uniform3fv(x.U.uMetal2, o.metal2 || [.95, .64, .54]);
    gl.uniform1i(x.U.uIcon, 0); gl.uniform1i(x.U.uRing, 1); gl.uniform1i(x.U.uBack, 2);
    gl.uniform1f(x.U.uIconOn, tx.I ? 1 : 0); gl.uniform1f(x.U.uRingOn, tx.R ? 1 : 0); gl.uniform1f(x.U.uBackOn, tx.B ? 1 : 0);
    gl.activeTexture(gl.TEXTURE2); gl.bindTexture(gl.TEXTURE_2D, tx.B);
    gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, tx.R);
    gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, tx.I);
  }
  /** One still frame of a coin onto `target` (the design cards), once its program is compiled. The live view redraws next frame. */
  function snap(target, o, yaw, pitch) {
    const G = root.YTPMGL;
    if (!G || !(gl = G.ctx())) return Promise.resolve(false);
    const x = prog(variant('coin', o));
    return Promise.all([G.when(x), coinTex(o), inRing(o) ? G.when(prog(IN_RING)) : true]).then(([ok, tx]) => {
      const drop = () => [tx.I, tx.B, tx.R].forEach((t) => t && gl && gl.deleteTexture(t));
      if (!ok || !gl || !target.isConnected) { drop(); return false; }
      const w = target.width, h = target.height;
      G.size(w, h);
      const M = mat(yaw, pitch, 0);
      gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
      const under = inRing(o) && bubblePass(M, w, h, 14);
      G.use(x);
      gl.uniform2f(x.U.uRes, w, h); gl.uniform1f(x.U.uTime, 9); gl.uniform1f(x.U.uKind, 0);
      gl.uniformMatrix3fv(x.U.uR, false, [M[0], M[3], M[6], M[1], M[4], M[7], M[2], M[5], M[8]]); gl.uniformMatrix3fv(x.U.uRT, false, M);
      coinUniforms(x, o, tx);
      if (under) { gl.enable(gl.BLEND); gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA); }
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      if (under) gl.disable(gl.BLEND);
      G.out(target);
      drop();
      dirty = true;
      return true;
    });
  }
  function start(canvas, k, reduced, o) {
    stop();
    const G = root.YTPMGL;
    if (!G || !(gl = G.ctx())) return false;
    gen++; cv = canvas; kind = k; still = !!reduced; opt = o || {}; wear = opt.wear || [0, 0, 0];
    if (texI) gl.deleteTexture(texI); if (texB) gl.deleteTexture(texB); if (texR) gl.deleteTexture(texR); texI = texB = texR = null;
    cur = null; want(variant(k, opt, wear));
    if (k === 'medal') medalTextures(opt, gl);
    if (k === 'coin') { const my = gen; coinTex(opt).then((x) => { if (my !== gen || !gl) { [x.I, x.B, x.R].forEach((tx) => tx && gl && gl.deleteTexture(tx)); return; } texI = x.I; texB = x.B; texR = x.R; dirty = true; }); }
    t0 = performance.now(); dirty = true; vis = true;
    if (!io && 'IntersectionObserver' in root) io = new IntersectionObserver((es) => es.forEach((e) => { if (e.target === cv) { vis = e.isIntersecting; if (vis) dirty = true; } }));
    if (io) io.observe(canvas);
    raf = requestAnimationFrame(frame);
    return true;
  }
  /** Orientation in degrees: yaw about the vertical axis, pitch about the horizontal axis (screen), spin about the stone's own axis. */
  function rot(yaw, pitch, spin) { R = mat(yaw, pitch, spin); dirty = true; }
  function mat(yaw, pitch, spin) {
    const a = yaw * D2R, b = pitch * D2R, s = (spin || 0) * D2R;
    const Ry = [Math.cos(a), 0, Math.sin(a), 0, 1, 0, -Math.sin(a), 0, Math.cos(a)];
    const Rx = [1, 0, 0, 0, Math.cos(b), -Math.sin(b), 0, Math.sin(b), Math.cos(b)];
    const Rz = [Math.cos(s), -Math.sin(s), 0, Math.sin(s), Math.cos(s), 0, 0, 0, 1];
    const mul = (A, B) => { const C = []; for (let i = 0; i < 3; i++) for (let j = 0; j < 3; j++) C[i * 3 + j] = A[i * 3] * B[j] + A[i * 3 + 1] * B[3 + j] + A[i * 3 + 2] * B[6 + j]; return C; };
    return mul(mul(Ry, Rx), Rz);   // row-major, object -> world
  }
  function stop() { if (raf) cancelAnimationFrame(raf); raf = 0; if (io && cv) io.unobserve(cv); cur = null; pending = null; cv = null; }   // the context and programs stay for next time
  root.YTPM3D = { start, rot, stop, snap, warm, redraw: () => { dirty = true; }, active: () => !!cv, canvas: () => cv, wear: (w) => { wear = [+w[0] || 0, +w[1] || 0, +w[2] || 0]; dirty = true; if (cv && (kind === 'medal' || kind === 'dia' || kind === 'ruby')) want(variant(kind, opt, wear)); } };
})(typeof window !== 'undefined' ? window : globalThis);
