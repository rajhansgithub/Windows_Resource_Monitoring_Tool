/* The age look (tuner values) and the shared dust + cobweb GLSL used by the 3D shaders. Loaded first: js/3d.js builds its shaders from it. */
(function (root) {
  'use strict';
  /** Age look (the full view's "Tune the age" sliders). Shader part goes to uAge[4]; the rest drives the wear clock and motion. */
  root.YTPMAgeDefaults = { dustAmt: 2, dustPatch: 1, dustUp: 1, dull: 1, grit: 1.5, lint: 1.29, webAmt: 1.3, webThreads: 1.4, webWidth: 1.42, webGlint: .65, webSheet: .3, webDust: 1.6, webRainbow: .19,
    dustDays: 527, webStart: 20, webSpan: 330, rubDust: .3, sway: .33, hover: .33, flick: 1, friction: 2.2 };
  root.YTPMAge = Object.assign({}, root.YTPMAgeDefaults, root.YTPMAge || {});
  root.YTPMAgeVec = () => { const a = root.YTPMAge; return new Float32Array([a.dustAmt, a.dustPatch, a.dustUp, a.dull, a.grit, a.lint, a.webAmt, a.webThreads, a.webWidth, a.webGlint, a.webSheet, a.webDust, a.webRainbow, 0, 0, 0]); };
  root.YTPMWearGLSL = `
// ---- age: dust and cobwebs (uWear = dust, unused, cobweb, 0..1). 1 real hour = 1 day on the shelf; polishing removes it.
// Dust is grains, not a film: a few grains first, more as it builds, until they join into a grainy grey layer (a year).
// It lies thick on flat surfaces and piles up in hollows (the engraving, the groove under the rim); steep walls stay
// almost clean and glossy; the top gets it before the bottom. Each grain is lit from the top left. Lint fibres lie on top.
// Cobweb: it grows, it never scales. Every thread has a fixed place and a birth time (from a hash); time only reveals
// them: first a few hair-thin threads from the rim, then more between them, drop lines with beads, then torn gauze
// sheets between the ropes and the rim. Dust thickens the threads into grey furry ropes and makes them sag.
// uWear.x = dust (the clock already applied the dust amount), .y = this medal's seed, .z = cobweb age.
// uAge = the tuner (YTPMAge): [0] (dust amount: used by the clock), patchiness, top first, hides reflections; [1] grit, lint, web amount,
// how many threads; [2] thread thickness, glint, gauze, dust on silk; [3] rainbow.
uniform vec3 uWear;
uniform vec4 uAge[4];
float wS, wC;   // this medal's seed (uWear.y, from its id) and the angle its cobweb hangs at: every medal gets its own web and dust
float wN(vec2 p){ vec2 i=floor(p), f=fract(p); vec2 u=f*f*(3.-2.*f); return mix(mix(hash(i),hash(i+vec2(1.,0.)),u.x),mix(hash(i+vec2(0.,1.)),hash(i+vec2(1.,1.)),u.x),u.y); }
float wF(vec2 p){ return .5*wN(p)+.25*wN(p*2.03+3.1)+.125*wN(p*4.01+7.7); }
vec2 wSeg(vec2 p, vec2 a, vec2 b){ vec2 pa=p-a, ba=b-a; float h=clamp(dot(pa,ba)/dot(ba,ba),0.,1.); return vec2(length(pa-ba*h),h); }
// short curled fibres, one per cell of size 1/sc. dens = how many of them exist yet (each has its own fixed place).
float wHair(vec2 p, float sc, float w, float aa, float dens){
  vec2 q=(p+(vec2(wN(p*11.+2.),wN(p*11.+8.))-.5)*.07)*sc, id=floor(q);
  float m=0.;
  for(int j=-1;j<=1;j++) for(int i=-1;i<=1;i++){
    vec2 c=id+vec2(float(i),float(j));
    if(hash(c+13.7)<dens){
      vec2 o=c+vec2(hash(c+3.1),hash(c+5.3));
      float an=hash(c+7.9)*3.1416;
      vec2 dir=vec2(cos(an),sin(an))*(.25+.45*hash(c+11.));
      float d=wSeg(q,o-dir,o+dir).x/sc;
      m=max(m,(1.-smoothstep(max(w-aa,0.)*.4,w+aa,d))*(.5+.5*hash(c+17.)));
    }
  }
  return m*min(1.,w*2.4/(aa*1.5+1e-4));
}
// rope i of the web: where it is tied (a on the rim; b on the rim for even i, on the face for odd i) and how far it sags
void wRope(float i, float G, out vec2 a, out vec2 b, out float sg){
  float sd=wS*61.7, h1=hash(vec2(i*1.31+.7+sd,2.9)), h2=hash(vec2(i*2.17+4.1,7.3+sd)), h3=hash(vec2(i*.73+9.2+sd,1.1));
  float a1=wC-1.175+2.35*fract(i*.381966+.13+wS*.37);
  a=vec2(cos(a1),sin(a1))*.955;
  float a2=clamp(a1+(h1>.5?1.:-1.)*(.6+.8*h2),wC-1.425,wC+1.425);
  vec2 f=vec2(-.55+.7*h1,.6-.7*h2); float r0=wC-2.175, cs=cos(r0), sn=sin(r0);
  b=mod(i,2.)<.5 ? vec2(cos(a2),sin(a2))*.955 : vec2(f.x*cs-f.y*sn,f.x*sn+f.y*cs);
  sg=(.05+.11*h3)*length(b-a)*(.5+.9*clamp((G-i*.055)*1.2,0.,1.));   // dust loads it: it sags more with age
}
// the point at t along rope i
vec2 wOn(float i, float t, float G){
  vec2 a,b; float sg; wRope(i,G,a,b,sg);
  vec2 cu=normalize(b-a), nn=vec2(-cu.y,cu.x); if(dot(nn,vec2(.35,-1.))<0.) nn=-nn;
  return mix(a,b,t)+nn*sg*4.*t*(1.-t);
}
// one thin sagging thread from a to b: uneven width, beads of dust, a soft fuzz. grow: 0..1+ how much of it is spun yet.
float wLine(vec2 p, vec2 a, vec2 b, float sg, float w, float aa, float seed, float grow, float fuzz){
  vec2 ch=b-a; float L=length(ch); vec2 cu=ch/L, nn=vec2(-cu.y,cu.x); if(dot(nn,vec2(.35,-1.))<0.) nn=-nn;
  float u=dot(p-a,cu)/L, h=dot(p-a,nn);
  float uc=clamp(u,0.,1.), s=sg*4.*uc*(1.-uc), ds=sg*4.*(1.-2.*uc)/L;
  float d=abs(h-s)/sqrt(1.+ds*ds);
  if(u<-.03||u>1.03||d>w*9.+aa*3.) return 0.;
  float ww=w*(.55+.9*wN(vec2(uc*L*26.,seed)))*(1.+1.6*smoothstep(.78,.95,wN(vec2(uc*L*64.,seed+3.))));
  float core=(1.-smoothstep(max(ww-aa,0.)*.4,ww+aa,d))*min(1.,ww*2.4/(aa*1.5+1e-4));
  float halo=(1.-smoothstep(0.,ww*4.5+aa*2.,d))*.22*fuzz*(.6+.8*wN(p*110.+seed));
  return max(core,halo)*smoothstep(-.02,.01,u)*(1.-smoothstep(.99,1.02,u))*(1.-smoothstep(grow-.08,grow,uc));
}
// the cobweb: rgb = its light (premultiplied, never above a), a = how much it covers
vec4 wWeb(vec2 p, float amt, float dust, float aa){
  if(amt<=.001 || dot(p,vec2(cos(wC),sin(wC)))<-.5) return vec4(0.);   // it lives on this medal's upper side (left, top or right)
  float G=min(amt,1.3), keep=.75*uAge[1].w, W=uAge[2].x;
  float rope=0., fine=0., gz=0., shd=0.;
  float thrH=.24+.18*smoothstep(.55,1.,G);                      // old gauze tears: the holes only grow
  for(int i=0;i<8;i++){
    float fi=float(i), age=G-fi*.055;
    if(age>0. && (i<2 || hash(vec2(fi+wS*53.,3.3))<keep)){
      vec2 a,b; float sg; wRope(fi,G,a,b,sg);
      vec2 ch=b-a; float L=length(ch); vec2 cu=ch/L, nn=vec2(-cu.y,cu.x); if(dot(nn,vec2(.35,-1.))<0.) nn=-nn;
      float u=dot(p-a,cu)/L, h=dot(p-a,nn);
      if(u>-.03 && u<1.03){
        float uc=clamp(u,0.,1.), s=sg*4.*uc*(1.-uc), ds=sg*4.*(1.-2.*uc)/L;
        float e=h-s, d=abs(e)/sqrt(1.+ds*ds);
        float w=(i<2?.016:.006+.008*hash(vec2(fi+wS*53.,8.8)))*W*(.12+.88*smoothstep(0.,.6,age))*(.6+.6*dust*uAge[2].w);
        shd=max(shd,(1.-smoothstep(0.,w*5.+.01,abs(e-.014-w)))*smoothstep(-.02,.03,u)*(1.-smoothstep(.97,1.02,u))*min(age*8.,1.));   // its soft shadow on the medal
        if(d<w*9.+aa*3.){
          float ww=w*(.55+.9*wN(vec2(uc*L*26.,fi*7.+1.)))*(1.+1.6*smoothstep(.78,.95,wN(vec2(uc*L*64.,fi*5.+3.))));
          float dr=d+(wN(p*75.+fi*3.)-.5)*ww*1.3;                // a ragged, furry outline
          float core=(1.-smoothstep(ww*.25,ww*1.15+aa,dr))*min(1.,ww*2.4/(aa*1.5+1e-4))*(.62+.3*wN(vec2(uc*L*40.,e*160.)+fi));
          float halo=(1.-smoothstep(ww*.5,ww*3.4+aa*2.,dr))*.42*smoothstep(0.,.4,age)*(.35+1.1*wN(p*130.+fi));   // fur
          rope=max(rope,max(core,halo)*smoothstep(-.02,.01,u)*(1.-smoothstep(.99,1.02,u))*(1.-smoothstep(age/.07-.08,age/.07,uc)));
        }
        float gage=age-.2-.2*hash(vec2(fi+wS*53.,6.1));               // gauze: later, on the rim-to-rim ropes
        if(gage>0. && u>0. && u<1.){
          float skirt=(.03+.16*wF(vec2(uc*L*6.,fi*3.+2.)))*min(gage*3.,1.);   // a ragged hem hangs below the rope
          float reg=e<0. ? (mod(fi,2.)<.5 ? smoothstep(0.,.05,.955-length(p)) : 0.) : 1.-smoothstep(skirt*.3,skirt,e);
          if(reg>0.){
            float holes=smoothstep(thrH-.12,thrH+.1,wF(p*5.5+fi*3.7+wS*9.));
            float fibre=(.55+.45*wN(vec2(uc*L*9.+fi,e*85.)))*(.7+.3*wN(p*70.));
            gz=max(gz,reg*holes*(.4+.6*fibre)*smoothstep(0.,.35,gage)*smoothstep(0.,.05,uc)*(1.-smoothstep(.95,1.,uc)));
          }
        }
      }
    }
  }
  for(int k=0;k<12;k++){                                       // fine threads from rope to rope
    float fk=float(k);
    float ia=floor(hash(vec2(fk+wS*53.,1.2))*7.99), ib=mod(ia+1.+floor(hash(vec2(fk+wS*53.,2.6))*2.99),8.);
    float age=G-max(.08+.065*fk,max(ia,ib)*.055+.03);
    if(age>0. && hash(vec2(fk+wS*53.,4.4))<keep && (ia<1.5||hash(vec2(ia+wS*53.,3.3))<keep) && (ib<1.5||hash(vec2(ib+wS*53.,3.3))<keep)){
      vec2 a=wOn(ia,.15+.7*hash(vec2(fk+wS*53.,5.5)),G), b=wOn(ib,.15+.7*hash(vec2(fk+wS*53.,6.6)),G);
      float m=wLine(p,a,b,.03*length(b-a),.0017*W*(.45+.55*smoothstep(0.,.5,age)),aa,fk*3.1,age/.06,smoothstep(0.,.5,age));
      if(G>.8 && hash(vec2(fk+wS*53.,9.9))>.7){ float u=wSeg(p,a,b).y; m*=smoothstep(.0,.1,abs(u-.5)-.1*smoothstep(.8,1.,G)); }   // an old one snaps
      fine=max(fine,m);
    }
  }
  for(int k=0;k<6;k++){                                        // drop lines hanging from the ropes, beaded with dust
    float fk=float(k), ia=floor(hash(vec2(fk+wS*53.,11.2))*7.99);
    float age=G-max(.05+.08*fk,ia*.055+.02);
    if(age>0. && hash(vec2(fk+wS*53.,14.4))<keep && (ia<1.5||hash(vec2(ia+wS*53.,3.3))<keep)){
      vec2 a=wOn(ia,.2+.6*hash(vec2(fk+wS*53.,15.5)),G);
      vec2 b=a+vec2((hash(vec2(fk+wS*53.,16.6))-.5)*.1,-(.09+.2*hash(vec2(fk+wS*53.,17.7))));
      fine=max(fine,wLine(p,a,b,.004,.0016*W*(.45+.55*smoothstep(0.,.5,age)),aa,fk*5.3+40.,age/.05,smoothstep(0.,.5,age)));
    }
  }
  float gza=clamp(gz*uAge[2].z*2.3,0.,.8);
  float al=clamp(max(rope,fine*.5)+gza*(1.-rope),0.,.94);
  vec3 silk=vec3(.86,.85,.83), dusty=vec3(.70,.69,.66);
  float lit=.86+.14*smoothstep(-1.,1.,p.y)+.12*uAge[2].y*fine*(1.-dust*.6)-.14*gza*(1.-rope);   // fresh silk glints; dusty silk does not
  vec3 rgb=mix(silk,dusty,clamp(uAge[2].w*(.45+.55*dust),0.,1.))*al*lit
          +uAge[3].x*max(rope,fine)*(1.-dust)*.3*(.5+.5*cos(6.283*(p.x*5.+p.y*3.+vec3(0.,.33,.67))));
  return vec4(min(rgb,vec3(al)),al+(1.-al)*max(shd*.4,gza*.15));
}
vec4 wOver(vec4 L, vec3 c, float k){ return vec4(L.rgb*(1.-k)+c*k,L.a+k-L.a*k); }   // a layer over a premultiplied layer
// The age of one spot, as layers over its colour that do not depend on the colour (so a still medal can work them out
// once and keep them): h = the haze (amount, tint, grey), L = dust, fibres and cobweb as one premultiplied layer.
// p: on the face (radius 1, +y up). recess: how sheltered (dust piles up there). flt: 1 on a flat surface, lower on a
// steep wall (dust slides off). False: nothing on it.
bool ageLayers(vec2 p, float recess, float flt, float aa, out vec3 h, out vec4 L){
  float dust=min(uWear.x,1.), web=uWear.z*uAge[1].z;
  wS=uWear.y; wC=2.175+(wS-.5)*2.;   // upper left on average; from the top right to the left side
  vec2 ps=p+wS*vec2(3.7,5.3);         // this medal's own dust pattern
  h=vec3(0.); L=vec4(0.);
  if(dust+web<.002) return false;
  float rc=min(recess,1.);
  // how much has settled here: the top first (the bottom's share = dust^(1+2*how low)), little on steep walls, more in hollows
  float low=1.-smoothstep(-1.,1.,p.y+(wF(p*2.5+3.)-.5)*.3);
  float dl=pow(max(dust,1e-4),1.+2.*low*uAge[0].z);
  float wall=1.-smoothstep(.72,.97,flt);
  float T=dl*mix(1.,mix(.05,.7,dl*dl),wall)*(1.+1.3*rc);
  float Tc=min(T,1.);
  // grains: clumps, coarse grains, fine grains. A grain is there once the layer passes its own threshold, so more dust =
  // more grains (none ever moves). Small tiles cannot show the fine ones (f1, f2 fade them out: no shimmer).
  float f1=clamp(2.-aa*38.,0.,1.), f2=clamp(2.-aa*95.,0.,1.), f3=clamp(1.6-aa*170.,0.,1.);
  float clump=wF(ps*4.3+11.)/.875;
  float g1=wN(ps*38.+1.), g2=wN(ps*95.+7.), g3=wN(ps*170.+4.);
  float n=mix(.5,clump,uAge[0].y)*.42+mix(.5,g1,f1)*.2+mix(.5,g2,f2)*.24+mix(.5,g3,f3)*.14;
  float thr=.80-.72*Tc;
  float cov=smoothstep(thr-.03,thr+.05,n)*smoothstep(0.,.04,T);
  float thick=smoothstep(0.,.3,n-thr);
  vec2 Ld=vec2(-.6,.8)*.006;                                  // towards the light
  float sh=clamp(1.+((g1-wN((ps+Ld)*38.+1.))*.35*f1+(g2-wN((ps+Ld*.5)*95.+7.))*.65*f2)*1.1,.78,1.22);
  // a thin layer: the mirror goes hazy (dust scatters the light), the colour still shows through (see ageApply)
  h=vec3(smoothstep(0.,.6,T)*uAge[0].w,(.17+.07*smoothstep(-1.,1.,p.y))*(1.-.12*rc),1.-.12*rc);
  // the grains themselves: light grey, each a little different, lit from the top left; dark specks and pale grains
  vec3 dc=vec3(.60,.585,.55);
  float tone=(.50+.10*smoothstep(-1.,1.,p.y)+.16*thick)*(.80+.22*mix(.5,g2,f2)+.22*mix(.5,g3,f3)+.25*(clump-.5)+.3*(wF(ps*13.+2.)-.44))*sh*(1.-.18*rc);
  vec3 dustc=dc*tone;
  dustc*=1.-.5*smoothstep(.86,.95,wN(ps*150.+3.))*f2*uAge[1].x;
  dustc+=dc*.22*smoothstep(.9,.97,wN(ps*60.+9.))*f1*uAge[1].x;
  L=wOver(L,dustc,cov*(.45+.52*thick));
  // lint: long fibres lie anywhere (on clean metal too), short fuzz grows in the thick dust
  float lint=wHair(ps,8.,.0022,aa,.4*smoothstep(.05,.7,dl)*uAge[1].y);
  float fuzz=wHair(ps+5.3,19.,.0017,aa,.8*smoothstep(.3,.9,T)*uAge[1].y);
  L=wOver(L,vec3(.72,.71,.68),lint*.5);
  L=wOver(L,vec3(.70,.69,.66),fuzz*.4);
  vec4 wb=wWeb(p,web,dust,aa);
  L=vec4(L.rgb*(1.-wb.a)+wb.rgb,L.a+wb.a-L.a*wb.a);
  return true;
}
// put the age layers on a premultiplied colour (coverage a)
vec3 ageApply(vec3 col, float a, vec3 h, vec4 L){
  float mx=max(max(col.r,col.g),max(col.b,.001));
  col=mix(col,(col/mx*h.y+vec3(.15,.145,.135)*h.z)*a,h.x);
  return col*(1.-L.a)+L.rgb*a;
}
// age a (premultiplied) colour (see ageLayers). metal: 1 metal, 0 stone.
vec3 aged(vec3 col, float a, vec2 p, float recess, float flt, float metal, float aa){
#ifndef AGED
  return col;
#else
  vec3 h; vec4 L;
  if(a<=0. || !ageLayers(p,recess,flt,aa,h,L)) return col;
  return ageApply(col,a,h,L);
#endif
}
`;
})(typeof window !== 'undefined' ? window : globalThis);
