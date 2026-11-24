/* Live materials for earned tiers 1-6 (reference gem.js, maths unchanged). One shared WebGL context draws every <canvas class="ach-gemgl" data-kind="bronze|silver|gold|platinum|dia|ruby" data-icon="...">.
   data-shape="round" (THE THEME, v2 Uptime Medals): a struck round medal (polished rim, spun face with fine rings, icon engraved) for metals and the round brilliant for Diamond / Ruby.
   Shapes plaque / award are kept for the earlier theme options (v1). uTilt (YTPMGem.set(c, {tilt})) moves the studio lights with the full view's pointer tilt.
   60 fps. YTPMGem.only(canvas) draws only that canvas (the full view) while the rest hold still. data-dpr caps the pixel ratio.
   No network. Pauses off screen and in a hidden tab; one still frame with "reduce motion". No WebGL: the parent gets .is-nogl. */
(function (root) {
  'use strict';
  const VS = 'attribute vec2 a;void main(){gl_Position=vec4(a,0.,1.);}';
  const FS = `
precision highp float;
uniform vec2 uRes; uniform float uTime; uniform float uSeed; uniform sampler2D uIcon; uniform float uIconOn; uniform float uPx; uniform sampler2D uDet; uniform float uDetOn; uniform vec2 uTilt;
const float PI=3.14159265;
mat2 rot(float a){ float c=cos(a), s=sin(a); return mat2(c,-s,s,c); }
float hash(vec2 p){ p=fract(p*vec2(123.34,456.21)); p+=dot(p,p+45.32); return fract(p.x*p.y); }
// studio: soft ceiling, strip lights round the side that turn slowly, hard spots for sparkle,
// and the viewer's own dark head straight above (this gives a brilliant its black-and-white mosaic)
float env(vec3 d, float t){
  d.xy=rot(t*.23)*d.xy;
  float az=atan(d.y,d.x);
  float s=.03+.45*smoothstep(-.1,.9,d.z);
  s+=1.1*pow(max(cos(az*3.)*.5+.5,0.),5.)*smoothstep(-.2,.5,d.z)*smoothstep(.98,.75,d.z);
  s+=pow(max(dot(d,normalize(vec3(.55,.55,.63))),0.),60.)*7.;
  s+=pow(max(dot(d,normalize(vec3(-.72,.25,.64))),0.),40.)*4.;
  s+=pow(max(dot(d,normalize(vec3(.05,-.78,.62))),0.),80.)*6.;
  s*=1.-.95*smoothstep(.94,.985,d.z);
  return s;
}
// light that enters through a facet (normal n), bounces off two pavilion mains and leaves through the crown.
// Worked out once per facet (from its centre c), so every facet is one clean flat tone, like a real stone.
float inner(vec2 c, vec3 v, vec3 n, float ior, float t){
  vec3 T=refract(-v,n,1./ior);
  if(dot(T,T)<.01) return 0.;
  vec2 q=c+T.xy*.7;
  float az=atan(q.y,q.x);
  float ps=(floor(az/(PI/4.))+.5)*(PI/4.);
  float th=radians(40.8);
  vec3 np=vec3(cos(ps)*sin(th),sin(ps)*sin(th),-cos(th));
  vec3 D1=reflect(T,np);
  vec2 q2=q+D1.xy*.8;
  float az2=atan(q2.y,q2.x);
  float ps2=(floor(az2/(PI/4.))+.5)*(PI/4.);
  vec3 np2=vec3(cos(ps2)*sin(th),sin(ps2)*sin(th),-cos(th));
  vec3 D2=reflect(D1,np2);
  if(D2.z<=0.) return .02;
  vec3 O=refract(D2,vec3(0.,0.,-1.),ior);
  if(dot(O,O)<.01) return .05;
  return env(normalize(O),t);
}
// ---- struck metal medals (tiers 1-4): polished raised rim, spun face with fine rings, icon engraved (frosted floor, bright cut walls)
uniform vec3 uMetal;

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
float envM(vec3 d, float t){
  d.xy=rot(t*.25)*d.xy;
  float s=.10+.30*smoothstep(-.3,1.,d.z);
  s+=1.5*pow(max(dot(d,normalize(vec3(.5,.45,.75))),0.),9.);
  s+=.7*pow(max(dot(d,normalize(vec3(-.65,-.25,.7))),0.),14.);
  s+=.35*smoothstep(.2,-.4,d.y)*smoothstep(-.6,.3,d.z);
  s+=4.*pow(max(dot(d,normalize(vec3(.45,.55,.7))),0.),220.);
  return s;
}
float icA(vec2 p){ vec2 iu=p/.66*.5+.5; return texture2D(uIcon,vec2(iu.x,1.-iu.y)).a; }
// studio for the metals: a dark room with a big softbox up left (close to the camera, so the face catches part of it),
// a tall strip light right, a warm floor bounce, a thin horizon line and one hard glint. Moves with the full view's tilt.
float envS(vec3 d, float t){
  d.xy=rot(.12*sin(t*.21)+uTilt.x*.8)*d.xy;
  d.yz=rot(uTilt.y*.6+.04*sin(t*.17))*d.yz;
  float s=.09+.12*smoothstep(-.2,.9,d.y);
  vec3 a=normalize(vec3(-.42,.46,.78));
  vec3 ux=normalize(cross(vec3(0.,0.,1.),a)), uy=cross(a,ux);
  float ca=dot(d,a);
  if(ca>0.){ vec2 q=vec2(dot(d,ux),dot(d,uy))/ca; s+=2.6*smoothstep(.08,-.08,max(abs(q.x)-.55,abs(q.y)-.40))*(.8+.2*smoothstep(-.3,.3,q.y)); }
  vec3 b=normalize(vec3(.82,.0,.57)); float cb=dot(d,b);
  if(cb>0.){ vec3 bx=normalize(cross(vec3(0.,1.,0.),b)); vec2 q=vec2(dot(d,bx),d.y)/cb; s+=1.8*smoothstep(.05,-.05,max(abs(q.x)-.08,abs(q.y)-1.)); }
  s+=.32*smoothstep(-.05,-.7,d.y)*smoothstep(-.4,.2,d.z);
  s+=.25*exp(-pow((d.y+.1)/.03,2.))*smoothstep(-.5,.2,d.z);
  s+=9.*pow(max(dot(d,normalize(vec3(.3,.62,.72))),0.),700.);
  return s;
}
vec3 aces(vec3 x){ return clamp((x*(2.51*x+.03))/(x*(2.43*x+.59)+.14),0.,1.); }
vec4 medal(vec2 p, float r, float aa, float t){
  vec3 base=uMetal;
  vec3 v=normalize(vec3(-p*.30,1.));                      // a near camera
  vec2 rd=r>1e-4?p/r:vec2(1.,0.);
  vec2 tg=vec2(-rd.y,rd.x);
  float RIM=.83;
  vec3 col;
  if(r>RIM){
    // polished raised rim: a rounded bead, then a crisp outer edge. Mirror finish: one sharp reflection.
    float x=clamp((r-RIM)/(.975-RIM),0.,1.);
    float sl=cos(x*PI)*1.25;
    sl=mix(sl,-2.6,smoothstep(.972,.99,r));
    vec3 n=normalize(vec3(-rd*sl,1.));
    vec3 R=reflect(-v,n);
    float cv=clamp(dot(n,v),0.,1.);
    vec3 F=base+(1.-base)*pow(1.-cv,5.);
    col=F*envS(R,t);
    col*=1.-.45*smoothstep(.16,0.,x);                    // the face's shadow where the bead rises
  } else {
    // spun face, very slightly domed: the rings smear each reflection along the radius (the bow-tie of real spun metal)
    vec3 n=normalize(vec3(p*.22,1.));
    float ring=sin(r*uPx*.9)*.5+.5;
    n=normalize(n+vec3(rd*(ring-.5)*.012,0.));
    float e=0.;
    for(int k=0;k<4;k++){ float o=(float(k)-1.5)/1.5; e+=envS(reflect(-v,normalize(n+vec3(rd*o*.28,0.))),t); }
    e*=.25;
    float cv=clamp(dot(n,v),0.,1.);
    vec3 F=base+(1.-base)*pow(1.-cv,5.);
    col=F*e*.92;
    col*=1.-.35*smoothstep(RIM-.12,RIM,r);              // occlusion under the rim
  }
  // a fine groove where the rim meets the face
  col*=1.-smoothstep(aa*1.5,0.,abs(r-RIM))*.7;
  // engraved icon: sunken, sandblasted floor (matte, darker) and polished walls that catch the studio
  if(uIconOn>0. && r<RIM){
    float d=1.6/max(uPx,40.);
    float m=icA(p);
    float gx=icA(p+vec2(d,0.))-icA(p-vec2(d,0.));
    float gy=icA(p+vec2(0.,d))-icA(p-vec2(0.,d));
    if(m+abs(gx)+abs(gy)>0.){
      float grain=.85+.3*hash(floor(gl_FragCoord.xy));
      float diff=.10+.10*envS(vec3(-.35,.4,.85),t);
      vec3 floorC=base*diff*grain;
      col=mix(col,floorC,smoothstep(.15,.85,m));
      vec3 nw=normalize(vec3(vec2(gx,gy)*2.2,1.));
      float w=clamp(length(vec2(gx,gy))*1.6,0.,1.);
      vec3 Rw=reflect(-v,nw);
      col=mix(col,base*envS(Rw,t)*1.1,w*.9);
    }
  }
  float inside=smoothstep(1.+aa,1.-aa,r);
  col=aces(col*1.1);
  return vec4(col*inside,inside);
}
float segd(vec2 p, vec2 a, vec2 b){ vec2 pa=p-a, ba=b-a; float h=clamp(dot(pa,ba)/dot(ba,ba),0.,1.); return length(pa-ba*h); }
// ---- plaque shapes (the owner 2026-11-21: metals as plaques; Diamond and Ruby in a plaque-like cut) ----
const vec2 PB=vec2(.97,.74);   // plaque half size (4:3)
float sdRBox(vec2 p, vec2 b, float rr){ vec2 q=abs(p)-b+rr; return length(max(q,0.))+min(max(q.x,q.y),0.)-rr; }
float icP(vec2 p, float k){ vec2 iu=p/k*.5+.5; return texture2D(uIcon,vec2(iu.x,1.-iu.y)).a; }
// metal plaque: polished bevelled frame, brushed face (fine horizontal lines), icon engraved
vec4 plaqueM(vec2 p, float aa, float t){
  vec3 base=uMetal;
  float d=sdRBox(p,PB,.2);
  float inside=smoothstep(aa,-aa,d);
  if(inside<=0.) return vec4(0.);
  vec3 v=normalize(vec3(-p*.3+vec2(.06*sin(t*.5),.05*sin(t*.37+1.)),1.));
  float e2=1.5/max(uPx,40.);
  vec2 g=normalize(vec2(sdRBox(p+vec2(e2,0.),PB,.2)-sdRBox(p-vec2(e2,0.),PB,.2), sdRBox(p+vec2(0.,e2),PB,.2)-sdRBox(p-vec2(0.,e2),PB,.2))+1e-6);
  float W=.16;                                  // frame width
  vec3 n=vec3(0.,0.,1.); bool frame=d>-W;
  if(frame){
    float x=clamp((d+W)/(W*.92),0.,1.);
    float sl=cos(x*PI)*1.05;
    sl=mix(sl,-2.3,smoothstep(-.03,-.008,d));
    n=normalize(vec3(g*sl,1.));
  } else {
    n=normalize(vec3(0.,(sin(p.y*uPx*1.1)*.5)*.05+(hash(vec2(floor(p.y*uPx*.55),3.))-.5)*.04,1.));   // brushed: fine horizontal hairlines
  }
  vec3 R=reflect(-v,n);
  float e=envM(R,t);
  vec3 col;
  if(frame) col=base*e*1.25+vec3(e*e*.04);
  else {
    // brushing smears the light into a soft band that slides across the face as the lights turn
    float la=t*.25+.7;
    float band=exp(-pow(dot(p,vec2(cos(la),sin(la)))-.35*sin(t*.3),2.)*5.);
    col=base*(e*.8+band*.45+.08);
  }
  col*=1.-smoothstep(aa*1.5,0.,abs(d+W))*.55;   // groove where the frame meets the face
  if(uIconOn>0. && !frame){
    float k=.34, dd=1.6/max(uPx,40.);   // icon at half the face
    float m=icP(p,k);
    float gx=icP(p+vec2(dd,0.),k)-icP(p-vec2(dd,0.),k);
    float gy=icP(p+vec2(0.,dd),k)-icP(p-vec2(0.,dd),k);
    if(m+abs(gx)+abs(gy)>0.){
      float grain=.82+.36*hash(floor(gl_FragCoord.xy));
      col=mix(col,base*(.40+.14*envM(vec3(0.,0.,1.),t))*grain,smoothstep(.15,.85,m));
      vec3 nw=normalize(vec3(vec2(gx,gy)*2.2,1.));
      col=mix(col,base*envM(reflect(-v,nw),t)*1.3,clamp(length(vec2(gx,gy))*1.6,0.,1.)*.9);
    }
  }
  col=1.-exp(-col*1.6);
  return vec4(col*inside,inside);
}
// emerald (step) cut, the plaque-shaped stone: a table and three rows of steps round a rectangle with cut corners
vec4 emerald(vec2 p, float aa, float t){
  float ruby=uKind;
  vec2 B=PB; float C=.30;                       // cut corners
  vec2 q=abs(p);
  float cx=q.x-B.x, cy=q.y-B.y, cc=(q.x+q.y-(B.x+B.y-C))*.7071;
  float d=max(max(cx,cy),cc);
  float inside=smoothstep(aa,-aa,d);
  if(inside<=0.) return vec4(0.);
  // which side: 0 = left/right, 1 = top/bottom, 2 = corner
  float side; vec2 o; float sec;
  if(cc>=cx && cc>=cy){ side=2.; o=normalize(sign(p+1e-6)); }
  else if(cx>=cy){ side=0.; o=vec2(sign(p.x+1e-6),0.); }
  else { side=1.; o=vec2(0.,sign(p.y+1e-6)); }
  sec=side*4.+(p.x>0.?1.:0.)+(p.y>0.?2.:0.);
  float top2=max(max(min(cx,cy),min(cx,cc)),min(cy,cc));   // second biggest: the line between two sides
  float ed=(d-top2)*.75;
  float T=.30;                                   // crown steps take this much from the edge
  vec3 n; float fid; vec2 cen;
  float tabT=-T;
  if(d>tabT){
    float band=clamp(floor((d-tabT)/T*3.),0.,2.);
    n=normalize(vec3(o*sin(.30+band*.2),cos(.30+band*.2)));
    fid=band*12.+sec;
    float lv=tabT+(band+.5)*T/3.;
    cen=p-o*0.;  cen=o*(1.+lv)*.8;
    ed=min(ed,abs(fract((d-tabT)/T*3.+.5)-.5)*T/3.);
  } else {
    // the table: you see the pavilion steps mirrored in it, nested rectangles (the "hall of mirrors")
    float k=floor((tabT-d)/.13);
    n=normalize(vec3(-o*sin(.07+.05*mod(k,3.)),1.));
    fid=60.+k*12.+sec;
    cen=o*(.62-k*.13)+vec2(-o.y,o.x)*.2*mod(k,2.);
    ed=min(ed,abs(fract((tabT-d)/.13+.5)-.5)*.13);
    ed=min(ed,abs(d-tabT));
  }
  vec3 v=normalize(vec3(.09*sin(t*.47),.08*sin(t*.31+1.),1.));
  n=normalize(n+vec3((hash(vec2(fid,3.1))-.5)*.08,(hash(vec2(fid,7.7))-.5)*.08,0.));
  float ior=mix(2.417,1.77,ruby);
  float F0=pow((ior-1.)/(ior+1.),2.);
  float F=F0+(1.-F0)*pow(1.-max(dot(n,v),0.),5.);
  float outer=env(reflect(-v,n),t);
  float disp=mix(.06,.02,ruby);
  vec2 cp=mix(cen,p,d>tabT?.15:0.);   // steps: one clean tone each (the table's mirrored steps too)
  vec3 inn=vec3(inner(cp,v,n,ior-disp,t), inner(cp,v,n,ior,t), inner(cp,v,n,ior+disp,t));
  vec3 col;
  if(ruby<.5){
    col=inn*(1.-F)*1.1 + outer*F*1.5;
    col=pow(col,vec3(1.45))*1.5+.015;
  } else {
    float lum=dot(inn,vec3(.33));
    float path=mix(.5,1.2,smoothstep(-.5,0.,d));
    vec3 absorb=exp(-vec3(.05,3.,2.4)*path);
    col=pow(vec3(lum),vec3(1.3))*(1.-F)*vec3(1.,.1,.17)*absorb*4.2;
    col+=vec3(.2,.006,.03)*(1.-.5*smoothstep(-.6,0.,d));
    col+=vec3(.75,.03,.09)*pow(.5+.5*sin(t*.8+hash(vec2(fid,9.))*6.28),3.)*.6;
    col+=pow(min(lum,3.),2.)*vec3(1.,.4,.45)*.05;
    col+=pow(outer*F,1.4)*vec3(1.,.9,.92)*1.6;
  }
  float line=smoothstep(aa*1.3,0.,ed)*smoothstep(-.005,-.03,d);
  col=mix(col,col*.6+mix(vec3(.08),vec3(.1,.02,.03),ruby),line*.55);
  col+=mix(vec3(.6,.62,.68),vec3(.7,.2,.26),ruby)*smoothstep(-.03,-.01,d)*smoothstep(0.,-.01,d);
  if(uIconOn>0. && d<tabT){
    float k=.56, px=1./max(uPx,48.);
    vec2 iu=p/k*.5+.5; vec2 fl=vec2(iu.x,1.-iu.y);
    float m=texture2D(uIcon,fl).a, m2=texture2D(uIcon,fl+vec2(px,px)*1.5).a;
    float tab=smoothstep(tabT,tabT-.04,d); m*=tab; m2*=tab;
    if(m+m2>0.){
      float grain=.85+.3*hash(floor(gl_FragCoord.xy));
      vec3 frost=(ruby<.5 ? vec3(.86,.89,.94) : vec3(1.,.52,.58))*(.6+.12*sin(t*.4+p.x*2.))*grain;
      col=mix(col,col*.35+frost,m*.78);
      col+=max(m-m2,0.)*mix(vec3(.7),vec3(.8,.4,.42),ruby)*.5;
      col*=1.-max(m2-m,0.)*.45;
    }
  }
  col=1.-exp(-max(col,0.)*1.6);
  return vec4(col*inside,inside);
}
vec3 tilt(float c, float a){ return normalize(vec3(cos(c)*sin(a),sin(c)*sin(a),cos(a))); }
// the round brilliant, drawn in p (|p| = 1 at the girdle); p0 + ik: where and how big the etched icon is (ik 0 = fill the table)
vec4 brill(vec2 p, float r, float aa, float t, vec2 p0, float ik){
  float inside=smoothstep(1.+aa,1.-aa,r);
  if(inside<=0.) return vec4(0.);
  float ruby=uKind;
  // crown facets (8-fold): local u along the sector's centre, w across
  float a=atan(p.y,p.x);
  float sec=floor((a+PI/8.)/(PI/4.));
  float ca=sec*PI/4.;
  float s=a-ca;
  float u=r*cos(s), w=r*sin(s);
  float TB=.58, SP=.81, tn=tan(PI/8.);
  vec2 lp=vec2(u,abs(w));
  vec2 tv=vec2(TB,TB*tn), st=vec2(SP,0.), g=vec2(cos(PI/8.),sin(PI/8.)), g0=vec2(1.,0.);
  float side=sign(w+1e-6);
  vec3 n; float fid; vec2 cen; float ed;
  bool table=u<TB;
  if(table){
    // through the table you see the pavilion mains and lower girdles mirrored: 16 wedges, split near the middle
    float wa=floor(a/(PI/8.)); float ring=step(.3+.04*sin(wa*2.1),r);
    fid=100.+wa+ring*16.;
    float cwa=(wa+.5)*PI/8.;
    cen=vec2(cos(cwa),sin(cwa))*mix(.18,.45,ring);
    n=tilt(cwa+PI,mix(.10,.22,ring));
    float wl=abs(sin(a*8.))*r;                     // distance to a wedge line
    ed=min(wl*.9, abs(r-.3-.04*sin(wa*2.1)));
    ed=min(ed,u-TB<0.?TB-u:1.);
  } else {
    ed=min(min(segd(lp,vec2(TB,0.),tv),segd(lp,tv,st)),min(segd(lp,st,g),segd(lp,st,g0)));
    if(abs(w)<(SP-u)/(SP-TB)*TB*tn){ n=tilt(ca,.27); fid=1.+sec; cen=vec2(cos(ca),sin(ca))*.65; }
    else {
      vec2 e=g-st; vec2 l2=lp-st;
      if(e.x*l2.y-e.y*l2.x<0.){ float c=ca+side*PI/16.; n=tilt(c,.74); fid=20.+sec*2.+side; cen=vec2(cos(c),sin(c))*.93; }
      else { float c=ca+side*PI/8.; n=tilt(c,.6); fid=40.+mod(sec*2.+(side>0.?1.:0.)+16.,16.); cen=vec2(cos(c),sin(c))*.8; }
    }
  }
  // the stone rocks a little; each facet is a touch off (real cutting), so they flash at different moments
  vec3 v=normalize(vec3(.09*sin(t*.47)+uTilt.x*.9,.08*sin(t*.31+1.)+uTilt.y*.9,1.));
  n=normalize(n+vec3((hash(vec2(fid,3.1))-.5)*.08,(hash(vec2(fid,7.7))-.5)*.08,0.));
  float ior=mix(2.417,1.77,ruby);
  float F0=pow((ior-1.)/(ior+1.),2.);
  float F=F0+(1.-F0)*pow(1.-max(dot(n,v),0.),5.);
  float outer=env(reflect(-v,n),t);
  // brilliance + fire: three colours, each with its own index (dispersion makes the rainbow flashes)
  float disp=mix(.06,.02,ruby);
  vec2 cp=mix(cen,p,.35);   // a little of the pixel's own place: soft shading across each facet
  vec3 inn=vec3(inner(cp,v,n,ior-disp,t), inner(cp,v,n,ior,t), inner(cp,v,n,ior+disp,t));
  vec3 col;
  if(ruby<.5){
    col=inn*(1.-F)*1.1 + outer*F*1.5;
    col=pow(col,vec3(1.45))*1.5+.015;                      // a real brilliant is mostly bright white and near black
  } else {
    // ruby: deep red body; the further light travels inside (towards the rim), the darker and richer it gets
    float lum=dot(inn,vec3(.33));
    float path=mix(.5,1.2,smoothstep(.15,1.,r));
    vec3 absorb=exp(-vec3(.05,3.,2.4)*path);
    col=pow(vec3(lum),vec3(1.3))*(1.-F)*vec3(1.,.1,.17)*absorb*4.2;
    col+=vec3(.2,.006,.03)*(1.-.5*r);
    // light bounced round the body lights crown facets one by one (a ruby's slow red 'breathing')
    if(!table) col+=vec3(.75,.03,.09)*pow(.5+.5*sin(t*.8+hash(vec2(fid,9.))*6.28),3.)*mix(1.,.45,smoothstep(.8,1.,r));                     // light scattered inside: a soft red glow
    col+=pow(min(lum,3.),2.)*vec3(1.,.4,.45)*.05;          // the brightest flashes lean pink-white
    col+=pow(outer*F,1.4)*vec3(1.,.9,.92)*1.6;
  }
  // facet lines: a touch darker with a hairline of light
  float line=smoothstep(aa*1.3,0.,ed)*smoothstep(.985,.94,r);
  col=mix(col,col*.6+mix(vec3(.08),vec3(.1,.02,.03),ruby),line*.55);
  // girdle: thin bright rim
  col+=mix(vec3(.6,.62,.68),vec3(.7,.2,.26),ruby)*smoothstep(.955,.985,r)*smoothstep(1.,.985,r);
  // the badge's icon, laser-etched into the table: frosted (matte, scatters light) with a fine cut edge.
  // Not used now (the owner 11-21: no etching on Diamond / Ruby, it looks bad): every caller passes ik<0.
  if(uIconOn>0. && ik>=0.){
    vec2 iu=(ik>0.?p0/ik:p/(TB*1.42))*.5+.5;   // plaque stones: a smaller icon (half)
    vec2 fl=vec2(iu.x,1.-iu.y);
    float m=texture2D(uIcon,fl).a;
    float px=1./max(uPx,48.);
    float m2=texture2D(uIcon,fl+vec2(px,px)*1.5).a;
    float tab=smoothstep(TB+.01,TB-.03,u);
    m*=tab; m2*=tab;
    if(m+m2>0.){
      float grain=.85+.3*hash(floor(gl_FragCoord.xy));
      vec3 frost=(ruby<.5 ? vec3(.86,.89,.94) : vec3(1.,.52,.58))*(.6+.12*sin(t*.4+p.x*2.))*grain;
      col=mix(col,col*.35+frost,m*(ik>0.?.92:.78));
      col+=max(m-m2,0.)*mix(vec3(.7),vec3(.8,.4,.42),ruby)*.5;   // cut edge catches light on one side
      col*=1.-max(m2-m,0.)*.45;                                  // and is in shadow on the other
    }
  }
  col=max(col,0.);
  col=1.-exp(-col*1.6);
  return vec4(col*inside,inside);
}
// ---- vertical award plaque (the owner 2026-11-21, from his photo of a real creator award): a solid plate with a crisp chamfered
// edge, a recessed mirror pocket in the upper part with a small thin emblem (the badge icon), etched text under it and the
// Playlist Pro mark + name at the bottom (uDet: white-on-clear picture of everything etched). Metals: all metal.
// Diamond / Ruby: dark gunmetal plate, the edge band made of the stone itself (small triangular facets), the stone set in the pocket.
const vec2 VB=vec2(.74,.98);
const vec2 PC=vec2(0.,.40); const vec2 PH=vec2(.34,.23);
float det(vec2 p){ vec2 u=p/1.04*.5+.5; return uDetOn>0.?texture2D(uDet,vec2(u.x,1.-u.y)).a:0.; }
// one small triangular stone facet (edge band of the stone plaques)
vec3 stoneTri(vec2 p, float d, vec2 g, float t, float ruby, out float ed){
  float cs=.052;
  vec2 s=vec2(p.x+p.y*.57735,p.y*1.1547)/cs;
  vec2 ce=floor(s), f=fract(s);
  float up=step(1.,f.x+f.y);
  ed=min(min(f.x,f.y),abs(1.-f.x-f.y))*cs*.8;
  vec2 cc=ce+mix(vec2(1./3.),vec2(2./3.),up);
  vec2 cen=vec2(cc.x-cc.y*.5,cc.y*.866)*cs;
  float fid=hash(ce*.731+up*vec2(.37,.71))*97.;   // per-facet id, no lines of look-alike facets
  vec3 v=normalize(vec3(.09*sin(t*.47)+uTilt.x*.9,.08*sin(t*.31+1.)+uTilt.y*.9,1.));
  float sl=mix(.25,.75,smoothstep(-.12,0.,d));                       // the band curves down to the edge
  vec3 n=normalize(vec3(g*sl+vec2(hash(vec2(fid,3.1))-.5,hash(vec2(fid,7.7))-.5)*.32,1.));
  float ior=mix(2.417,1.77,ruby);
  float F0=pow((ior-1.)/(ior+1.),2.);
  float F=F0+(1.-F0)*pow(1.-max(dot(n,v),0.),5.);
  float outer=env(reflect(-v,n),t);
  float disp=mix(.06,.02,ruby);
  vec3 inn=vec3(inner(cen,v,n,ior-disp,t),inner(cen,v,n,ior,t),inner(cen,v,n,ior+disp,t));
  vec3 col;
  if(ruby<.5){ col=inn*(1.-F)*1.1+outer*F*1.5; col=pow(col,vec3(1.45))*1.5+.015; }
  else {
    float lum=dot(inn,vec3(.33));
    col=pow(vec3(lum),vec3(1.3))*(1.-F)*vec3(1.,.1,.17)*exp(-vec3(.05,3.,2.4)*.8)*4.2;
    col+=vec3(.2,.006,.03)+vec3(.75,.03,.09)*pow(.5+.5*sin(t*.8+hash(vec2(fid,9.))*6.28),3.)*.6;
    col+=pow(outer*F,1.4)*vec3(1.,.9,.92)*1.6;
  }
  col=mix(col,col*.6+mix(vec3(.08),vec3(.1,.02,.03),ruby),smoothstep(.006,0.,ed)*.55);
  return col;
}
// pavé edge (the owner 2026-11-21: round stones of mixed sizes, "no gaps", "no straight edges"): round brilliants from small
// to big packed tight all the way round the edge (one continuous loop, so no seams at the corners or the middle); both
// the outer outline and the inner line against the plate follow the stones. Where two stones overlap, the one in front
// wins (power diagram), with a fine dark seam.
const float PW=.30;                               // how far in the stones can reach
float perim(){ return 4.*(VB.x+VB.y); }
// a point on the edge loop (clockwise from the top-left corner), w in from the edge
vec2 loopPt(float s, float w){
  s=mod(s,perim());
  float a=2.*VB.x, b=2.*VB.y;
  if(s<a) return vec2(-VB.x+s, VB.y-w);
  s-=a; if(s<b) return vec2(VB.x-w, VB.y-s);
  s-=b; if(s<a) return vec2(VB.x-s, -VB.y+w);
  s-=a; return vec2(-VB.x+w, -VB.y+s);
}
float loopS(vec2 p){
  vec2 dd=VB-abs(p);
  float a=2.*VB.x, b=2.*VB.y;
  if(dd.y<dd.x) return p.y>0. ? p.x+VB.x : a+b+(VB.x-p.x);
  return p.x>0. ? a+(VB.y-p.y) : 2.*a+b+(VB.y+p.y);
}
// the shape of the stone border: round lobes of mixed sizes all the way round the edge (one continuous loop), melted
// together into ONE piece (smooth union), so both the outside and the inside line are wavy, never straight
float vn(vec2 p){ vec2 i=floor(p), f=fract(p); f=f*f*(3.-2.*f);
  return mix(mix(hash(i),hash(i+vec2(1,0)),f.x),mix(hash(i+vec2(0,1)),hash(i+vec2(1,1)),f.x),f.y); }
// how deep the stone border reaches in at p: a wavy line, big slow waves plus smaller ripples (2D noise, so no seams at
// the corners)
float paveW(vec2 p){ return .13+.13*vn(p*3.1+vec2(4.2,1.7))+.05*vn(p*8.3+vec2(9.1,3.3)); }
float paveSdf(vec2 p){
  float depth=-sdRBox(p,VB,.05);
  float ow=.055*vn(p*3.3+vec2(2.,7.))+.025*vn(p*9.1+vec2(5.,1.));                 // the outside line is wavy too (the owner 11-21: no straight outer edge)
  return max(ow-depth, depth-paveW(p));
}
// the owner 2026-11-21: "made up of a single piece": the border is one carved piece of diamond / ruby, cut all over in small
// triangular facets, domed so the facets tip towards its wavy edges
vec4 pave(vec2 p, float d, float aa, float t, vec3 v){
  float sd=paveSdf(p);
  if(sd>aa) return vec4(0.);
  float e=1.5/max(uPx,40.);
  vec2 g=normalize(vec2(paveSdf(p+vec2(e,0.))-sd, paveSdf(p+vec2(0.,e))-sd)+1e-6);
  float ed;
  vec3 col=stoneTri(p,-.02,g*smoothstep(-.06,0.,sd)*1.4,t,uKind,ed);   // one piece: flat top, rounded only at its own edge
  col+=mix(vec3(.5,.52,.58),vec3(.6,.15,.2),uKind)*smoothstep(-.012,-.002,sd);   // a bright line along the polished edge
  col=1.-exp(-max(col,0.)*mix(2.6,1.6,uKind));   // diamond a little brighter
  float a=smoothstep(aa,-aa,sd);
  return vec4(col*a,a);
}
vec4 vplaque(vec2 p, float aa, float t){
  bool stone=uKind<1.5;
  float ruby=uKind;
  vec3 base=stone?vec3(.13,.13,.15):uMetal;
  float dO=sdRBox(p,VB,.05);
  if(dO>aa) return vec4(0.);
  float d=stone?dO+.1:dO;                          // stone plaques: the plate sits inside, the stones make the outline
  float inside=smoothstep(aa,-aa,d);
  vec3 v=normalize(vec3(-p*.3+uTilt*.9+vec2(.05*sin(t*.5),.04*sin(t*.37+1.)),1.));
  vec4 pv=stone&&dO>-PW?pave(p,dO,aa,t,v):vec4(0.);
  if(pv.a>=.999) return pv;
  float e2=1.5/max(uPx,40.);
  vec2 g=normalize(vec2(sdRBox(p+vec2(e2,0.),VB,.05)-sdRBox(p-vec2(e2,0.),VB,.05), sdRBox(p+vec2(0.,e2),VB,.05)-sdRBox(p-vec2(0.,e2),VB,.05))+1e-6);
  float W=.035;
  vec3 col; bool edge=d>-W;
  {
    vec3 n;
    if(edge) n=normalize(vec3(g*1.6,1.));
    else n=normalize(vec3((hash(vec2(floor(p.y*uPx*.6),3.))-.5)*.03,sin(p.y*uPx*1.1)*.02,1.));   // brushed: fine horizontal lines
    float e=envM(reflect(-v,n),t);
    if(edge) col=base*e*1.35+vec3(e*e*.05);
    else {
      float la=t*.25+.7+uTilt.x*2.;
      float sh=exp(-pow(dot(p,vec2(cos(la),sin(la)))-.35*sin(t*.3)-uTilt.y,2.)*4.);
      col=base*(e*(stone?.8:.62)+sh*.38+.05)+(stone?vec3(e*e*.025+sh*.03):vec3(0.));
    }
  }
  if(!edge){
    // etched text and the Playlist Pro mark: frosted (matte, lighter), fine walls. The light it catches is capped: a moving
    // highlight once made the text flash bright white for a moment (the owner 11-21 "text is blown out, then fixes")
    float m=det(p)*smoothstep(80.,180.,uPx);          // text is only etched where it can be read (big badges, the full view)
    if(m>0.){
      float grain=.85+.3*hash(floor(gl_FragCoord.xy));
      vec3 fr=(stone?vec3(.62,.63,.67):mix(base,vec3(1.),.5))*(.85+.12*min(envM(vec3(.3,.4,1.),t),1.2))*grain;
      col=mix(col,fr,m*.92);
    }
  }
  // the pocket: recessed, mirror polished
  vec2 pe=p-PC;
  float dp=sdRBox(pe,PH,.07);
  vec4 top=vec4(0.);
  if(stone){
    // the owner 11-21: no grey pocket round the stone, only the diamond / ruby, set straight on the plate with a soft shadow
    vec2 SH=PH-.03; vec2 q=abs(pe);
    float gg=max(max(q.x/SH.x,q.y/SH.y),(q.x+q.y)/(SH.x+SH.y-.12));
    col*=1.-.55*smoothstep(1.16,.98,gg-pe.y*.15);
    if(gg<1.02){
      float an=atan(pe.y/SH.y,pe.x/SH.x);
      top=brill(gg*vec2(cos(an),sin(an)),gg,aa/SH.y*1.3,t,pe,-1.);
    }
  } else if(dp<0.){
    vec2 gp=normalize(vec2(sdRBox(pe+vec2(e2,0.),PH,.07)-sdRBox(pe-vec2(e2,0.),PH,.07), sdRBox(pe+vec2(0.,e2),PH,.07)-sdRBox(pe-vec2(0.,e2),PH,.07))+1e-6);
    float wall=smoothstep(-.03,-.022,dp);
    vec3 n=normalize(vec3(-gp*1.4*wall,1.));
    float e=envM(reflect(-v,n),t);
    vec3 pb=stone?vec3(.8,.81,.86):base;
    col=pb*(e*.9)*mix(.75,1.25,wall)+vec3(e*e*.05);
    if(stone){
      if(dp<-.03){
        vec2 SH=PH-.03; vec2 q=abs(pe);
        float gg=max(max(q.x/SH.x,q.y/SH.y),(q.x+q.y)/(SH.x+SH.y-.12));
        float an=atan(pe.y/SH.y,pe.x/SH.x);
        top=brill(gg*vec2(cos(an),sin(an)),gg,aa/SH.y*1.3,t,pe,-1.);
      }
    } else {
      // the emblem: the badge icon, small and thin, raised and frosted like the emblem on a real award
      float m=det(p);
      if(m>0.){
        float grain=.88+.24*hash(floor(gl_FragCoord.xy));
        col=mix(col,(base*.7+vec3(.28))*(.8+.15*min(envM(vec3(.2,.5,1.),t),1.2))*grain,m*.95);
      }
    }
  } else col*=1.-.35*smoothstep(.03,0.,dp);
  col=1.-exp(-max(col,0.)*1.6);
  col=col*(1.-top.a)+top.rgb;
  vec4 pl=vec4(col*inside,inside);
  return pv+pl*(1.-pv.a);
}
void main(){
  float S=min(uRes.x,uRes.y);
  vec2 p=(gl_FragCoord.xy-.5*uRes)/(.5*S)*1.04;
  float r=length(p);
  float aa=2./S*1.04;
  float t=uTime+uSeed;
  if(uShape>1.5){ gl_FragColor=vplaque(p,aa,t); return; }
  if(uShape>.5){
    if(uKind>1.5){ gl_FragColor=plaqueM(p,aa,t); return; }
    // radiant cut (the plaque-shaped stone): the brilliant's triangles and kites drawn into a rectangle with clipped corners
    vec2 q=abs(p);
    float g=max(max(q.x/PB.x,q.y/PB.y),(q.x+q.y)/(PB.x+PB.y-.30));
    float an=atan(p.y/PB.y,p.x/PB.x);
    gl_FragColor=brill(g*vec2(cos(an),sin(an)),g,aa*1.3,t,p,-1.); return;
  }
  if(r>1.+aa){ gl_FragColor=vec4(0.); return; }
  vec4 c=uKind>1.5?medal(p,r,aa,t):brill(p,r,aa,t,p,-1.);
  float rec=uKind>1.5 ? (uIconOn>0. && r<.83 ? icA(p) : 0.)+smoothstep(.1,0.,abs(r-.83))*.8+smoothstep(.62,.83,r)*step(r,.83)*.3 : smoothstep(.86,1.,r)*.6;
  float fl=1.-.3*smoothstep(.5,1.,r);                 // how flat it is here: dust slides off the steep parts
  if(uKind>1.5){
    fl=1.;
    if(r>.83){ float sl=cos(clamp((r-.83)/(.975-.83),0.,1.)*PI)*1.25; fl=1./sqrt(1.+sl*sl); }          // the flanks of the rim bead
    else if(uIconOn>0. && uWear.x>0.){ float e=max(aa,.008); vec2 g=vec2(icA(p+vec2(e,0.))-icA(p-vec2(e,0.)),icA(p+vec2(0.,e))-icA(p-vec2(0.,e))); fl=1./sqrt(1.+dot(g,g)*5.); }   // the walls of the engraving
  }
  c.rgb=aged(c.rgb,c.a,p,rec,fl,uKind>1.5?1.:0.,aa);
  gl_FragColor=c;
}`;

  // metal colours (linear-ish reflectance): bronze, silver, gold, and platinum with the cool tint from the tier colour
  const METAL = { bronze: [0.86, 0.52, 0.30], silver: [0.93, 0.94, 0.97], gold: [1.0, 0.77, 0.33], platinum: [0.72, 0.93, 0.91] };
  const SHAPE = { round: 0, plaque: 1, award: 2 };
  const kindOf = (k) => (k === 'ruby' ? 1 : k === 'dia' ? 0 : METAL[k] ? 2 : 0);
  let gl = null, off = false, items = [], raf = 0, last = 0, io = null, t0 = 0, broken = false;
  let solo = null;
  const icons = new Map();   // icon key -> { tex, ok }
  const still = () => !!(root.matchMedia && root.matchMedia('(prefers-reduced-motion: reduce)').matches);
  let iconSrc = () => '';    // set by the page: icon name -> inner SVG markup (24x24, stroke)
  function setup() {
    if (gl && !gl.isContextLost()) return true;
    if (broken) return false;
    gl = root.YTPMGL ? root.YTPMGL.ctx() : null;
    if (!gl) { broken = true; return false; }
    off = root.YTPMGL.offscreen();
    if (!t0) {
      t0 = performance.now();
      root.YTPMGL.onLost(() => { icons.clear(); items.forEach((it) => { it.det = null; it.fresh = true; }); gl = null; });
      if ('IntersectionObserver' in root) io = new IntersectionObserver((es) => es.forEach((e) => { const it = items.find((x) => x.c === e.target); if (it) { it.vis = e.isIntersecting; if (it.vis) it.fresh = true; } }));
    }
    return true;
  }
  /** One program per feature set: stone kind, shape, aged or clean (#defines; the unused parts are not compiled at all). */
  const UN = ['uRes', 'uTime', 'uSeed', 'uIcon', 'uIconOn', 'uPx', 'uMetal', 'uDet', 'uDetOn', 'uTilt', 'uWear', 'uAge'];
  const isAged = (it) => it.wear[0] > 0 || (it.wear[2] || 0) > 0;
  const keyOf = (k, shape, aged) => 'gem2d:' + k + shape + (aged ? 'a' : '');
  function prog(k, shape, aged) {
    return root.YTPMGL.program(keyOf(k, shape, aged), FS, `#define uKind ${k}.\n#define uShape ${shape}.\n${aged ? '#define AGED\n' : ''}`, UN, (y) => {
      if (!y.ok) return;
      root.YTPMGL.use(y); gl.uniform1i(y.U.uIcon, 0); gl.uniform1i(y.U.uDet, 1);
      items.forEach((i) => { if (keyOf(i.kind, i.shape, isAged(i)) === y.key) i.fresh = true; });
      if (still()) draw(performance.now());
    });
  }
  /** Award plaques: everything etched on the plate (the badge icon small and thin in the pocket, the text lines, the
      Playlist Pro mark + name at the bottom), drawn white on a clear canvas (1024 px for the full view, 512 / 256 for smaller badges) and used as one texture (mipmapped). */
  const MARK_LINES = 'M4 12h3.6l2-3.7 1.8 7.3 1.8-8.6 1.7 5h5.1', MARK_PLAY = '';
  function detail(it) {
    const key = [it.icon, it.pre, it.title, it.line, it.sub, it.kind].join('|');
    if (it.det && it.det.key === key) return it.det;
    const rec = { key, tex: it.det && it.det.tex, ok: !!(it.det && it.det.ok) };
    it.det = rec;
    const N = it.px >= 400 ? 1024 : it.px >= 160 ? 512 : 256, P = (x, y) => [(x / 1.04 * 0.5 + 0.5) * N, (0.5 - y / 1.04 * 0.5) * N], U = (h) => h / 2.08 * N;
    const font = '"Chakra Petch", "IBM Plex Sans", "Segoe UI", system-ui, sans-serif';
    const paint = (img) => {
      if (rec !== it.det) return;
      const c = document.createElement('canvas'); c.width = N; c.height = N;
      const x = c.getContext('2d');
      x.fillStyle = '#fff'; x.strokeStyle = '#fff'; x.textAlign = 'center'; x.textBaseline = 'middle';
      const fit = (txt, h, w, weight) => { let s = U(h); x.font = `${weight} ${s}px ${font}`; while (x.measureText(txt).width > U(w) && s > 8) { s -= 1; x.font = `${weight} ${s}px ${font}`; } };
      const text = (txt, y, h, weight) => { if (!txt) return; fit(txt, h, it.kind > 1.5 ? 1.2 : 0.86, weight); const [cx, cy] = P(0, y); x.fillText(txt, cx, cy); };
      if (img && it.kind > 1.5) { const s = U(0.27); const [cx, cy] = P(0, 0.40); x.drawImage(img, cx - s / 2, cy - s / 2, s, s); }
      text(it.pre, 0.03, 0.058, 500);
      text(it.title, -0.08, 0.092, 700);
      text(it.line, -0.19, 0.056, 500);
      text(it.sub, -0.28, 0.05, 500);
      // the Playlist Pro mark (round, list + play knocked out) and the name, where the platform's logo sits on a real award
      x.font = `700 ${U(0.085)}px ${font}`;
      const name = 'Resource Monitor', tw = x.measureText(name).width, R = U(0.055), gap = U(0.03), w = R * 2 + gap + tw;
      const [lx, ly] = P(0, it.kind > 1.5 ? -0.74 : -0.6);   // stone plaques: above the pavé
      const mx = lx - w / 2 + R;
      x.beginPath(); x.arc(mx, ly, R, 0, Math.PI * 2); x.fill();
      x.save(); x.globalCompositeOperation = 'destination-out'; x.translate(mx - R, ly - R); x.scale(R * 2 / 24, R * 2 / 24);
      x.lineWidth = 2; x.lineCap = 'round'; x.stroke(new Path2D(MARK_LINES)); x.fill(new Path2D(MARK_PLAY)); x.restore();
      x.textAlign = 'left'; x.fillText(name, mx + R + gap, ly + U(0.004));
      try {
        rec.tex = rec.tex || gl.createTexture();
        gl.activeTexture(gl.TEXTURE1);
        gl.bindTexture(gl.TEXTURE_2D, rec.tex);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, c);
        gl.generateMipmap(gl.TEXTURE_2D);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        gl.activeTexture(gl.TEXTURE0);
        rec.ok = true; it.fresh = true;
        if (still()) draw(performance.now());
      } catch (e) { /* plain plate */ }
    };
    const go = () => {
      if (!it.icon) return paint(null);
      const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="256" height="256" fill="none" stroke="#fff" color="#fff" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${iconSrc(it.icon) || ''}</svg>`;
      const img = new Image();
      img.onload = () => paint(img); img.onerror = () => paint(null);
      img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    };
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(go, go); else go();
    return rec;
  }
  /** The icon as a texture: its SVG drawn white on a 512 px canvas (an SVG data image never taints the canvas), mipmapped.
      The edge is softened to ~4 texels so the engraved walls keep the width tuned at 128 px, without texel steps in the full view. */
  function icon(name) {
    if (icons.has(name)) return icons.get(name);
    const rec = { tex: null, ok: false };
    icons.set(name, rec);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="512" height="512" fill="none" stroke="#fff" color="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${iconSrc(name) || ''}</svg>`;
    const img = new Image();
    img.onload = () => {
      try {
        const c = document.createElement('canvas'); c.width = 512; c.height = 512;
        const x = c.getContext('2d');
        if ('filter' in x) x.filter = 'blur(1.4px)';
        x.drawImage(img, 0, 0, 512, 512);
        rec.tex = gl.createTexture();
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, rec.tex);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, c);
        gl.generateMipmap(gl.TEXTURE_2D);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        rec.ok = true;
        for (const it of items) if (it.icon === name) it.fresh = true;
        if (still()) draw(performance.now());
      } catch (e) { /* plain stone */ }
    };
    img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    return rec;
  }
  // Frame budget (shared with the other engines, YTPMGL.left): badges draw round-robin from `cur`; a frame stops when the
  // budget is spent or after `cap` badges, the rest draw next frame. `cap` follows the real frame gap (GPU cost included).
  let cur = 0, cap = 64, gap = 16;
  function draw(now) {
    if (!setup()) return;
    const t = still() ? 9 : (now - t0) / 3000;   // a third of real time: calm (the owner 11-22)
    const ageV = root.YTPMAgeVec ? root.YTPMAgeVec() : null;
    const n = items.length, anim = !still(), G = root.YTPMGL;
    let drawn = 0;
    for (let k = 0; k < n; k++) {
      const it = items[(cur + k) % n];
      if (solo && it.c !== solo) continue;            // full view open: only the big one draws, the rest hold still
      if (!it.vis && !it.fresh) continue;
      if (anim && !solo && drawn && (drawn >= cap || G.left(now) <= 0)) { cur = (cur + k) % n; return; }
      const x = prog(it.kind, it.shape, isAged(it));
      if (!x || !x.ok) continue;                      // still compiling: it stays fresh and draws when ready
      drawn++;
      it.fresh = false;
      G.use(x); G.size(it.px, it.px);
      const U = x.U;
      gl.uniform2f(U.uRes, it.px, it.px); gl.uniform1f(U.uTime, t); gl.uniform1f(U.uSeed, it.seed); gl.uniform1f(U.uPx, it.px); gl.uniform3fv(U.uMetal, METAL[it.metal] || METAL.gold); gl.uniform2f(U.uTilt, it.tilt[0], it.tilt[1]); gl.uniform3f(U.uWear, it.wear[0], it.wear[1], it.wear[2] || 0); if (U.uAge && ageV) gl.uniform4fv(U.uAge, ageV);
      const dt = it.shape === 2 ? detail(it) : null;
      gl.uniform1f(U.uDetOn, dt && dt.ok ? 1 : 0);
      if (dt && dt.ok) { gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, dt.tex); gl.activeTexture(gl.TEXTURE0); }
      const ic = it.icon ? icon(it.icon) : null;
      gl.uniform1f(U.uIconOn, ic && ic.ok ? 1 : 0);
      if (ic && ic.ok) { gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, ic.tex); }
      gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      G.out(it.c);
    }
  }
  function loop(now) {
    raf = 0;
    items = items.filter((it) => it.c.isConnected);
    if (!items.length) return;
    if (!document.hidden && now - last > 12) {   // 60 fps (the owner 11-21: 30 felt laggy)
      if (last && now - last < 200) { gap = gap * 0.8 + (now - last) * 0.2; cap = gap > 24 ? Math.max(2, Math.floor(cap * 0.8)) : gap < 19 ? Math.min(64, cap + 1) : cap; }
      draw(now); last = now;
    }
    if (!still()) raf = requestAnimationFrame(loop);
  }
  /** Start every canvas.ach-gemgl under `scope` that is not running yet. */
  function mount(scope) {
    const list = Array.from((scope || document).querySelectorAll('canvas.ach-gemgl:not([data-live])'));
    if (!list.length) return 0;
    if (!setup()) { list.forEach((c) => { c.dataset.live = '0'; if (c.parentElement) c.parentElement.classList.add('is-nogl'); }); return 0; }
    const dpr = Math.min(root.devicePixelRatio || 1, 2);
    for (const c of list) {
      const css = parseFloat(c.getAttribute('data-size')) || c.clientWidth || 40;
      const px = Math.max(16, Math.round(css * Math.min(dpr, parseFloat(c.dataset.dpr) || 2)));
      c.width = px; c.height = px; c.dataset.live = '1';
      if (c.parentElement) c.parentElement.classList.add('is-gl');
      items.push({ c, px, kind: kindOf(c.dataset.kind), metal: c.dataset.kind, shape: SHAPE[c.dataset.shape] || 0, icon: c.dataset.icon || '', pre: c.dataset.pre || '', title: c.dataset.title || '', line: c.dataset.line || '', sub: c.dataset.sub || '', tilt: [0, 0], wear: [0, 0, 0], det: null, seed: (items.length % 7) * 5.1, vis: true, fresh: true });
      if (io) io.observe(c);
    }
    if (still()) draw(performance.now());
    else if (!raf) raf = requestAnimationFrame(loop);
    return list.length;
  }
  /** Compile the programs the badge grid will need (called a few seconds after load; parallel compile keeps the page free). */
  function warm(list) { if (!setup()) return; (list || [[2, 0, 1], [2, 0, 0], [0, 0, 1], [1, 0, 1]]).forEach((v) => prog(v[0], v[1], v[2])); }
  /** Change a running stone: { icon, kind }. */
  function set(c, o) {
    const it = items.find((x) => x.c === c);
    if (!it) return false;
    if (o.icon !== undefined) { it.icon = o.icon || ''; c.dataset.icon = it.icon; }
    if (o.kind) { it.kind = kindOf(o.kind); it.metal = o.kind; c.dataset.kind = o.kind; }
    if (o.shape !== undefined) { it.shape = SHAPE[o.shape] || 0; c.dataset.shape = o.shape; }
    if (o.tilt) it.tilt = [+o.tilt[0] || 0, +o.tilt[1] || 0];
    if (o.wear) it.wear = [+o.wear[0] || 0, +o.wear[1] || 0, +o.wear[2] || 0];
    for (const k of ['pre', 'title', 'line', 'sub']) if (o[k] !== undefined) it[k] = o[k];
    it.fresh = true;
    if (still()) draw(performance.now());
    return true;
  }
  /** Draw only this canvas (the full view), at full frame rate; null = back to all. */
  function only(c) { solo = c || null; items.forEach((it) => { it.fresh = true; }); if (still()) draw(performance.now()); else if (!raf && items.length) raf = requestAnimationFrame(loop); }
  const api = { mount, set, only, warm, icons: (fn) => { iconSrc = fn; root.YTPMIconSrc = fn; }, supported: () => setup() };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  root.YTPMGem = api;
})(typeof window !== 'undefined' ? window : globalThis);
