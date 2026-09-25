/* app.js — the painting half. Everything that touches a document or the network is here;
 * everything that turns beads into a view model is in model.js.
 *
 * WHAT CHANGED FROM THE SPIKE THIS PAGE GREW OUT OF. The spike shipped its payload
 * pre-chewed — treemap rectangles, bead coordinates, connected components, execution layers
 * and every histogram arrived computed, and the page only drew them. That decision was
 * reversed on a measurement rather than an argument: the whole pass this file runs, layout
 * and routing included, is single-digit milliseconds at a few hundred beads and tens of
 * milliseconds at twenty thousand. Server-side layout bought nothing and cost a rendering
 * stack, so the server returns beads and this file decides where they go.
 */
'use strict';

/* THE ENDPOINT IS RELATIVE, so the page works wherever it is mounted — a subpath behind a
   tunnel is not a special case anybody would remember to test. `?api=` overrides it, which
   is how the page is opened against a saved payload without a server running. */
var API = (function () {
    try {
        var q = new URLSearchParams(location.search).get('api');
        if (q) return q;
    } catch (e) { /* file:// with no search string */ }
    return 'api/beads';
}());

var BOOT = document.getElementById('boot'), BOOTMSG = document.getElementById('bootmsg');

/* A FAILED READ SAYS WHAT FAILED, and never renders an empty page. An empty stage and a
   stage whose endpoint is down look identical, and the empty one reads as "no work", which
   is the one reading that stops anybody looking (law-absence-needs-a-positive-control). */
function bootFail(what, detail) {
    BOOT.classList.remove('hidden');
    BOOTMSG.innerHTML = '';
    var b = document.createElement('b'); b.textContent = what;
    var c = document.createElement('code'); c.textContent = detail;
    var r = document.createElement('button'); r.textContent = 'retry';
    r.onclick = function () { location.reload(); };
    BOOTMSG.appendChild(b); BOOTMSG.appendChild(document.createElement('br'));
    BOOTMSG.appendChild(c); BOOTMSG.appendChild(document.createElement('br'));
    BOOTMSG.appendChild(r);
}

var t0 = (typeof performance !== 'undefined' ? performance : Date).now();
fetch(API, { headers: { accept: 'application/json' }, cache: 'no-store' })
    .then(function (r) {
        if (!r.ok) throw new Error(API + ' answered HTTP ' + r.status + ' ' + r.statusText);
        return r.json();
    })
    .then(function (payload) {
        /* From here on, a throw is OURS. The catch below is scoped to the read, so anything
           this block raises is re-labelled before it reaches the overlay. */
        var read = (typeof performance !== 'undefined' ? performance : Date).now() - t0;
        var p = LoomModel.unwrap(payload);
        /* `meta` is how an installation that renamed its escalation or CI label reaches this
           page. Absent, the shipped defaults apply; present, it wins. A page with those
           labels written in shows fewer escalations and fewer parked beads on any
           installation that renamed them, and shows them as ordinary work. */
        var t1 = (typeof performance !== 'undefined' ? performance : Date).now();
        /* `edges` when the server lifted them out of the rows, `now` from the snapshot's own
           clock rather than this browser's — a machine whose clock is minutes off would
           otherwise draw the whole graph a different colour. */
        var model = LoomModel.derive(p.beads, Object.assign({}, p.meta, {
            edges: p.edges,
            now: p.meter.generatedAtMs || undefined
        }));
        var derive = (typeof performance !== 'undefined' ? performance : Date).now() - t1;
        BOOT.classList.add('hidden');
        try {
            boot(model, { beads: p.beads.length, read: read, derive: derive, meter: p.meter });
        } catch (e) {
            bootFail('the beads read fine; painting them did not',
                     String((e && e.stack) || e).replace(/\s+/g, ' ').slice(0, 400));
        }
    })
    .catch(function (e) { bootFail('cannot read the beads', String(e && e.message || e)); });

function boot(D, timing) {
let B=D.beads, IDS=Object.keys(B);
const HEAT=['--h0','--h1','--h2','--h3','--h4','--h5','--h6'];
const heat=a=>`var(${a<=0?HEAT[0]:a<=1?HEAT[1]:a<=3?HEAT[2]:a<=7?HEAT[3]:a<=14?HEAT[4]:a<=30?HEAT[5]:HEAT[6]})`;
const S={view:'map',repo:null,focus:null,edges:'cone',saved:null,scale:'live',since:0,agg:'ex',zoom:null,chn:12};
const $=s=>document.querySelector(s), $$=s=>[...document.querySelectorAll(s)];
/* Every interaction is a URL, in both directions — the state is written to the hash
   on every render, so it has to be readable back off it or the link is decorative. */
try{const q=new URLSearchParams(location.hash.replace(/^#/,''));
  for(const [k,v] of q){ if(!(k in S)) continue;
    S[k]= k==='since'?+v : k==='saved'?(v===''||v==='null'?null:+v) : (v===''||v==='null'?null:v);}
}catch(e){}
const esc=s=>String(s).replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));
let st=D.stats; const THRESH=st.threshold||3;

/* ============ the density rule ============
   ρ is the constant, not the radius. Solve r = √(A·ρ / πn) over the stage,
   then shrink until every epic tile actually packs its beads, and stop at the
   legibility floor rather than shrinking past the point a ring reads as a ring. */
const VBW=1000, VBH=545, AREA=VBW*VBH;   /* the stage aspect once the chrome has taken its share */
/* Map type in SCREEN pixels. Everything else on the page is sized in px; the stage is
   a viewBox, so a size written in user units renders at whatever the stage happens to
   be scaled to — 11px on a wide window and 3px on a phone. PXK converts. */
const TXT={repo:16,rct:12,epic:15,agg:12};
let PXK=1;
const px=v=>v*PXK;
const RHO=0.32;      /* target ink fraction — one tuned constant, not a per-view guess */
const RFLOOR=5.0;    /* ~10 units of diameter; below this the ring encoding stops working */
const GAP=0.34;      /* centre pitch = 2r(1+GAP); ceiling on ink is π/4(1+GAP)² = 44% */

/* ---------- vitals ---------- */
/* paintBar: update the top bar's numbers. Called on first load and on every auto-refresh
   so the counts reflect the current corpus without a manual reload. */
function paintBar(tm){
  const rejN=IDS.filter(id=>B[id].att>=THRESH).length;
  const chnN=IDS.filter(id=>B[id].rec>0).length;
  $('#v-live').textContent=st.live; $('#v-mov').textContent=st.moving;
  $('#v-def').textContent=st.deferred; $('#v-cold').textContent=st.cold;
  $('#v-rej').textContent=rejN; $('#v-chn').textContent=chnN;
  /* p50 TIME IN FLIGHT, not p50 cycle time. Cycle time is created-to-closed and needs the
     closed population, which the read path does not carry; this is created-to-now over the
     beads still open, so it is a lower bound and is labelled as a different thing. */
  $('#v-cyc').textContent=D.flight.p50<48?Math.round(D.flight.p50)+'h':Math.round(D.flight.p50/24)+'d';
  $('#gen').textContent=new Date(D.generated).toISOString().slice(0,16).replace('T',' ')+' UTC';
  /* MEASURED, NOT ASSERTED. The mockup printed a fixed string here, which is a claim about
     somebody else's box; these are this load's own numbers. `derived` covers the whole view
     model — components, layers, grouping and every bucket — and it is the number that
     decided the client paints. */
  /* THE QUERY'S TIME AND ITS BUDGET COME FROM THE SERVER when it reports them, because a
     round trip measured here also contains the browser's queue and the network, and the
     number that says when reading on every request has stopped being cheap is the query's
     own. Falling back to the round trip is labelled as the round trip. */
  $('#cost').textContent=tm.beads+' beads · '+
    (tm.meter&&tm.meter.queryMs!==undefined
      ? 'query '+Math.round(tm.meter.queryMs)+'ms / '+Math.round(tm.meter.budgetMs)+'ms budget'
      : 'read '+Math.round(tm.read)+'ms')+
    ' · derived '+Math.round(tm.derive)+'ms';
}
paintBar(timing);
$('#ramp').innerHTML=['0d','1d','3d','1w','2w','30d','older']
  .map((l,i)=>`<i style="background:var(${HEAT[i]})" title="${l}"></i>`).join('')
  +`<span style="margin-left:5px">today → cold</span>`;

/* ---------- rail ---------- */
$('#repos').innerHTML=D.repos.map(r=>{
  const ags=r.epics.flatMap(e=>e.beads).map(id=>B[id].age);
  const seg=[0,1,3,7,14,30,999].map((t,i,arr)=>{
    const lo=i?arr[i-1]:-1, n=ags.filter(a=>a>lo&&a<=t).length;
    return n?`<i style="flex:${n};background:var(${HEAT[i]})"></i>`:''}).join('');
  return `<button class="rrow" data-repo="${r.repo}" aria-pressed="false">
    <span class="nm">${r.repo}</span><span class="ct">${r.n}</span><span class="hbar">${seg}</span></button>`}).join('');
const SAVED=[
 ['rejected 3+',b=>b.att>=THRESH],['worker churn',b=>b.rec>0],['poisoned',b=>b.poi],
 ['moving now',b=>b.s==='in_progress'],['needs operator',b=>b.nr],
 ['cold · 14d+',b=>b.age>14],['p0 / p1',b=>b.p<=1],
 ['in a chain',b=>b.up.length||b.dn.length],['deferred',b=>b.s==='deferred']];
$('#saved').innerHTML=SAVED.map(([n,f],i)=>
 `<button class="sv" data-saved="${i}" aria-pressed="false"><span>${n}</span><b>${IDS.filter(id=>f(B[id])).length}</b></button>`).join('');

const inScope=(id,bs)=>{
  const b=(bs||B)[id];
  if(S.repo&&b.repo!==S.repo) return false;
  if(S.saved!==null&&!SAVED[S.saved][1](b)) return false;
  return true;};
const moved=b=>b.age<=S.since;

/* ---------- cone ---------- */
function cone(id){
  if(!id||!B[id]) return null;
  const up=new Set(),dn=new Set();
  (function w(n,set,key){for(const m of B[n][key]){if(!set.has(m)){set.add(m);w(m,set,key);}}})(id,up,'up');
  (function w(n,set,key){for(const m of B[n][key]){if(!set.has(m)){set.add(m);w(m,set,key);}}})(id,dn,'dn');
  return {up,dn,all:new Set([...up,...dn,id])};}

/* ============ corpus: live, or synthetic at scale ============
   The hardest constraint on this surface is legibility at ten times the data, and
   the only honest way to hold it is to look. The synthetic corpora replay the LIVE
   distribution at 1k, 5k and 20k, so the density rule is measured rather than
   argued about — a formula that looked right on paper was rejected this way. */
let REAL={beads:B,components:D.components,repos:D.repos.map(r=>({repo:r.repo,n:r.n,
  epics:r.epics.map(e=>({id:e.id,title:e.title,n:e.n,beads:e.beads}))}))};
const SCALE={live:st.live,'1k':1000,'5k':5000,'20k':20000};
const mulberry=a=>()=>{a|=0;a=a+0x6D2B79F5|0;let t=Math.imul(a^a>>>15,1|a);
  t=t+Math.imul(t^t>>>7,61|t)^t;return((t^t>>>14)>>>0)/4294967296;};
let synCache={};
function corpus(){
  if(S.scale==='live') return REAL;
  if(synCache[S.scale]) return synCache[S.scale];
  const N=SCALE[S.scale], f=N/st.live, rnd=mulberry(0x10AA), proto=IDS.map(id=>B[id]);
  const beads={}, repos=[]; let k=0;
  for(const r of REAL.repos){
    const rn=Math.max(1,Math.round(r.n*f));
    const ne=Math.max(1,Math.round(r.epics.length*Math.sqrt(f)));
    const wt=Array.from({length:ne},(_,i)=>1/(i+1)), ws=wt.reduce((a,b)=>a+b,0);
    const epics=[]; let left=rn;
    for(let i=0;i<ne&&left>0;i++){
      let n=i===ne-1?left:Math.max(1,Math.round(rn*wt[i]/ws));
      n=Math.min(n,left); left-=n;
      const src=r.epics[i%r.epics.length], ids=[];
      for(let j=0;j<n;j++){
        const p=proto[Math.floor(rnd()*proto.length)], id='syn-'+(k++);
        beads[id]={t:p.t,repo:r.repo,s:p.s,ty:p.ty,p:p.p,age:p.age,born:p.born,ep:src.id,asg:p.asg,
          nr:p.nr,poi:p.poi,att:p.att,rec:p.rec||0,ci:p.ci,d:'',up:[],dn:[]};
        ids.push(id);}
      epics.push({id:src.id+'~'+i,title:src.title,n:ids.length,beads:ids});}
    repos.push({repo:r.repo,n:rn,epics});}
  /* Chains needs a blocking graph or it cannot be scale-tested at all. Replicate the
     LIVE component shapes — node layers and edges intact, ids remapped — so the shapes
     under test are the ones the harness actually produces, only more of them. */
  const pool=Object.keys(beads), comps=[];
  const chained=Math.round(N*D.components.reduce((s,c)=>s+c.size,0)/st.live);
  let used=0, q=0;
  while(used<chained&&pool.length-used>4){
    const src=D.components[q++%D.components.length];
    if(used+src.size>pool.length) break;
    const map={}; src.nodes.forEach((nd,j)=>{map[nd.id]=pool[used+j];});
    comps.push({size:src.size,depth:src.depth,width:src.width,
      nodes:src.nodes.map(nd=>({id:map[nd.id],l:nd.l,r:nd.r})),
      edges:src.edges.map(([x,z])=>[map[x],map[z]])});
    used+=src.size;}
  return synCache[S.scale]={beads,repos,components:comps};
}

/* ---------- treemap: recursive balanced split, longer axis first ---------- */
function tmap(items,x,y,w,h,out){
  if(!items.length||w<=0||h<=0) return;
  if(items.length===1){out.push({ref:items[0].ref,x,y,w,h});return;}
  const tot=items.reduce((s,i)=>s+i.v,0);
  let acc=0,k=1,best=Infinity;
  for(let i=1;i<items.length;i++){acc+=items[i-1].v;
    const d=Math.abs(acc/tot-0.5); if(d<best){best=d;k=i;}}
  const a=items.slice(0,k), b=items.slice(k);
  const fr=a.reduce((s,i)=>s+i.v,0)/tot;
  if(w>=h){tmap(a,x,y,w*fr,h,out); tmap(b,x+w*fr,y,w*(1-fr),h,out);}
  else    {tmap(a,x,y,w,h*fr,out); tmap(b,x,y+h*fr,w,h*(1-fr),out);}
}
/* ============ bead-first layout ============
   The first cut sized epic tiles by bead count and then packed beads into them,
   and the ink never rose above 11% of a 35% target at any scale: one badly-shaped
   tile caps the radius for every tile, and a grid inside a thin rectangle throws
   away most of its area to truncation. The treemap was the dead space, not the
   radius rule. So invert it — size each block to hold exactly its beads at the
   solved radius, then flow the blocks — and dead space becomes constant by
   construction rather than by hope. */
const GAPB=3, LB_SM=1.5, MAXRW=14;
/* The title band is a function of the bead, not a constant: at r=15 a 7.5-unit band
   renders the epic name at about seven pixels, and the beads sit on top of it. */
const lbFor=(r,tall)=>tall?Math.min(46,Math.max(7.5,px(TXT.epic)*1.45)):LB_SM;
/* Justified rows. Every block in a row is given the same number of bead-rows, so
   the row's blocks are exactly the same height and their widths add up to the
   stage width. Free-flowing blocks at their own natural aspect left 60% of each
   row empty, which is the same dead space the rule exists to remove. */
function rowFit(items,i,rw,p,W){
  let w=0,k=0;
  /* A row is only viable if its widest block fits the stage: without this the
     height objective happily picks one-deep rows and clips the largest epic. */
  if(Math.ceil(items[i].ref.n/rw)*p+3>W-6) return {k:0,w:0};
  while(i+k<items.length){
    const cw=Math.ceil(items[i+k].ref.n/rw)*p+3+GAPB;
    if(w+cw>W-4&&k>0) break;
    w+=cw; k++;}
  return {k,w};
}
function shelf(groups,r,W){
  const p=2*r*(1+GAP), flat=[];
  for(const g of groups){
    const es=g.epics.slice().sort((x,z)=>z.n-x.n);
    es.forEach((e,i)=>flat.push({ref:e,repo:g.repo,rn:g.n,first:i===0}));}
  const blocks=[],bands=[]; let y=1.5, i=0;
  while(i<flat.length){
    /* Choose the row's depth by how many beads it seats per unit of stage height,
       not by how neatly it fills the width: height is the budget that binds. */
    let best={k:1,rw:1},bs=-Infinity;
    for(let rw=1;rw<=MAXRW;rw++){
      const f=rowFit(flat,i,rw,p,W); if(!f.k) continue;
      let seat=0; for(let j=0;j<f.k;j++) seat+=flat[i+j].ref.n;
      const lbx=lbFor(r,rw*p>15);
      const sc=seat/(rw*p+lbx+GAPB+12);
      if(sc>bs){bs=sc;best={k:f.k,rw};}}
    const rw=best.rw, lb=lbFor(r,rw*p>15);
    /* A repo's blocks wrap across rows, so heading only its first block leaves every
       continuation row unlabelled. Head the start of a repo's run in each row, and
       mark the ones that are continuations. */
    const heads=[], hdr=Math.min(34,Math.max(10,px(TXT.repo)*1.35)), ly=y, top=y+hdr;
    let x=3;
    for(let j=0;j<best.k;j++){
      const it=flat[i+j], cols=Math.ceil(it.ref.n/rw);
      if(j===0||flat[i+j-1].repo!==it.repo) heads.push({repo:it.repo,n:it.rn,x,cont:!it.first});
      blocks.push({ref:it.ref,repo:it.repo,cols,rows:rw,lb,x,y:top,w:cols*p+3,h:rw*p+lb+1.5});
      x+=cols*p+3+GAPB;}
    /* Two repos whose first blocks are one narrow column apart ran their headings
       into each other; each heading gets the room up to the next one. */
    heads.forEach((hd,q)=>{hd.room=(q+1<heads.length?heads[q+1].x:x)-hd.x-5;});
    bands.push({heads,y:ly,hdr});
    y=top+rw*p+lb+1.5+GAPB+2; i+=best.k;}
  return {blocks,bands,height:y};
}
/* Largest radius at which every block still fits on the stage. */
function solveShelf(groups,W,H){
  let lo=0.35,hi=30;
  if(shelf(groups,hi,W).height<=H) return hi;
  for(let i=0;i<40;i++){const m=(lo+hi)/2; if(shelf(groups,m,W).height<=H) lo=m; else hi=m;}
  return lo;
}

function buildLayout(){
  const C=corpus(), bs=C.beads;
  let groups=C.repos, zoom=null;
  if(S.zoom){
    for(const g of C.repos){
      if(g.repo===S.zoom) zoom={kind:'repo',repo:g.repo,groups:[g]};
      for(const e of g.epics) if(e.id===S.zoom)
        zoom={kind:'epic',repo:g.repo,epic:e,groups:[{repo:g.repo,n:e.n,epics:[e]}]};}
    if(zoom) groups=zoom.groups;
  }
  const n=groups.reduce((s,g)=>s+g.epics.reduce((a,e)=>a+e.n,0),0);
  const rTarget=Math.sqrt(AREA*RHO/(Math.PI*Math.max(n,1)));
  const rFit=solveShelf(groups,VBW,VBH);
  const r=Math.min(rTarget,rFit);
  const L=shelf(groups,r,VBW);
  const fill=n*Math.PI*r*r/AREA;
  /* r scales as 1/√n, so the count at which the fit radius reaches the floor: */
  const nFloor=Math.round(n*Math.pow(rFit/RFLOOR,2));
  /* three resolutions, and the level is chosen, never guessed at by the reader */
  const epicCount=groups.reduce((s,g)=>s+g.epics.length,0);
  const level = r>=RFLOOR ? 'bead' : (epicCount<=120 ? 'epic' : 'repo');
  /* bead positions on the solved pitch */
  /* Centre each bead in its cell rather than butting it against the block edge.
     The state rings reach past r, so an r-inset put the top row's rings through
     the epic title and left a half-cell of slack on the other side. */
  const pos={}, cellOf={}, p=2*r*(1+GAP);
  for(const b of L.blocks){
    const cl=b.x+1.5, ct=b.y+b.lb;
    b.ref.beads.forEach((id,i)=>{
      const row=Math.floor(i/b.cols), col=i%b.cols;
      pos[id]=[cl+p/2+col*p, ct+p/2+row*p];
      cellOf[id]={blk:b.ref.id,row,col,cl,ct,cols:b.cols,rows:b.rows};});}
  /* aggregate tiling: area is bead count again, because nothing is packed into it */
  const agg=[];
  if(level!=='bead'){
    const items = level==='epic'
      ? groups.flatMap(g=>g.epics.map(e=>({v:Math.max(e.n,1),ref:{...e,repo:g.repo}})))
      : groups.map(g=>({v:Math.max(g.n,1),ref:{id:g.repo,title:g.repo,repo:g.repo,n:g.n,
          beads:g.epics.flatMap(e=>e.beads)}}));
    tmap(items.sort((a,b)=>b.v-a.v),1,1,VBW-2,VBH-2,agg);
  }
  /* exceptions get their own packing at the floor radius, so what is drawn stays readable */
  const posEx={}, over={};
  const pe=2*RFLOOR*(1+GAP);
  for(const t of agg){
    const ce=Math.max(0,Math.floor((t.w-3)/pe)), re_=Math.max(0,Math.floor((t.h-9)/pe));
    const ex=t.ref.beads.filter(id=>isException(bs[id]));
    t.ex=ex.length;
    t.exBy={rej:0,chn:0,poi:0};
    for(const id of ex){const q=bs[id];
      if(q.poi)t.exBy.poi++; else if(q.att>=THRESH)t.exBy.rej++; else t.exBy.chn++;}
    /* A mat of nine hundred alarm dots is not noticing, it is a second bead map.
       Draw them individually only while they can be counted by eye; past that the
       tile carries the counts and the drill-down carries the marks. */
    t.drawEx = ex.length<=Math.min(ce*re_,36);
    if(!t.drawEx){ if(ex.length) over[t.ref.id]=ex.length; continue; }
    ex.forEach((id,i)=>{
      posEx[id]=[t.x+1.5+pe/2+(i%ce)*pe, t.y+8+pe/2+Math.floor(i/ce)*pe];});}
  return {bs,blocks:L.blocks,bands:L.bands,agg,n,r,rFit,rTarget,level,nFloor,fill,pos,cellOf,p,posEx,over,zoom};
}

/* ---------- how a bead is drawn: state as form ----------
   Every bead occupies the SAME footprint, whatever its state. Decorations stack
   inward from the circumference and the core shrinks to make room, so a ringed
   bead reads as the same object in the same cell rather than a bigger one — the
   earlier version hung its rings outside r, which put the top row through the
   epic title and let two ringed neighbours collide. */
function beadSVG(b,id,x,y,r,dim,alarmOnly){
  const T=r*.15, G=r*.06;                        /* band thickness, band gap */
  const bands=[];
  if(moved(b)&&!alarmOnly) bands.push(['var(--cyan)',T*.62,false]);
  if(b.rec>0)              bands.push(['var(--brass)',T,true]);
  if(b.att>=THRESH)        bands.push(['var(--verm)',T,false]);
  let cur=r-T/2, extra='';
  for(const [col,w,dash] of bands){
    /* Dash count, not dash length, is what has to stay constant: a fixed length
       gives eight dashes on a big bead and a starburst on a small one. */
    const c=2*Math.PI*cur, da=dash?` stroke-dasharray="${(c/13).toFixed(2)} ${(c/19).toFixed(2)}"`:'';
    extra+=`<circle cx="${x}" cy="${y}" r="${cur.toFixed(2)}" fill="none" stroke="${col}"
      stroke-width="${Math.max(.35,w).toFixed(2)}"${da}/>`;
    cur-=T+G;}
  const core=bands.length?Math.max(r*.3,cur+T/2+G/2):r;
  let fill=heat(b.age),stroke='none',sw=0;
  if(b.s==='deferred'){fill='transparent';stroke=heat(b.age);sw=core*.5;}
  if(b.nr){fill='var(--violet)';}
  if(b.s==='in_progress'){fill='var(--jade)';stroke='var(--jade)';sw=core*.7;}
  if(b.poi){fill='var(--verm)';}
  if(id===S.focus){stroke='var(--ink)';sw=core*.6;}
  return `<g class="bead${dim?' dim':''}" data-id="${id}">${extra}<circle cx="${x}" cy="${y}"
    r="${core.toFixed(2)}" fill="${fill}" stroke="${stroke}" stroke-width="${sw.toFixed(2)}"/></g>`;
}
const isException=b=>b.att>=THRESH||b.rec>0||b.poi;

/* ============ orthogonal edge routing ============
   A bezier bowed from one bead to another passes straight through whatever sits
   between them — on a serpentine chain the row-wrap arc crosses most of the next
   row. Beads sit on a grid, so the gaps between them are a channel grid too:
   half a pitch from any bead centre, 2r·GAP wide. Route every edge along those
   channels and it stays clear of every node, the way a schematic does. */
function roundPath(pts,cr){
  if(pts.length<2) return '';
  let d=`M${pts[0][0].toFixed(1)} ${pts[0][1].toFixed(1)}`;
  for(let i=1;i<pts.length-1;i++){
    const [px,py]=pts[i-1],[cx,cy]=pts[i],[nx,ny]=pts[i+1];
    const ax=cx-px,ay=cy-py,bx=nx-cx,by=ny-cy;
    const la=Math.hypot(ax,ay)||1, lb=Math.hypot(bx,by)||1;
    const q=Math.min(cr,la/2,lb/2);
    d+=`L${(cx-ax/la*q).toFixed(1)} ${(cy-ay/la*q).toFixed(1)}`
      +`Q${cx.toFixed(1)} ${cy.toFixed(1)} ${(cx+bx/lb*q).toFixed(1)} ${(cy+by/lb*q).toFixed(1)}`;}
  const e=pts[pts.length-1];
  return d+`L${e[0].toFixed(1)} ${e[1].toFixed(1)}`;
}
/* Which channel an edge uses — computed once, so the track allocator and the router
   agree. They disagreed before: the allocator keyed on min(row)|min(col), which a
   row-wrap and the first horizontal edge of the row above both hash to, so the wrap
   got a different track in each row. */
function edgeCase(a,b,L){
  const A=L.cellOf[a], B=L.cellOf[b];
  if(!A||!B||!L.pos[a]||!L.pos[b]) return null;
  if(A.blk!==B.blk) return {kind:'x',key:'x'};
  const dr=B.row-A.row, dc=B.col-A.col;
  if(dr===0&&Math.abs(dc)===1) return {kind:'hdirect',dr,dc};
  if(dc===0&&Math.abs(dr)===1) return {kind:'vdirect',dr,dc};
  if(dr===0){const i=A.row+1; return {kind:'row',idx:i,key:`h${A.blk}:${i}`,dr,dc};}
  if(Math.abs(dr)===1){const i=Math.max(A.row,B.row); return {kind:'row',idx:i,key:`h${A.blk}:${i}`,dr,dc};}
  const cc=dc!==0?Math.max(A.col,B.col):(A.col>0?A.col:A.col+1);
  return {kind:'col',idx:cc,key:`v${A.blk}:${cc}`,dr,dc};
}
function routeEdge(a,b,L,off,cs){
  const A=L.cellOf[a], B=L.cellOf[b], pa=L.pos[a], pb=L.pos[b], r=L.r, p=L.p;
  const sy=cs.dr>0?1:-1;
  switch(cs.kind){
    case 'x':{                            /* out of one block, across, into the other */
      const my=(pa[1]+pb[1])/2+off;
      return [[pa[0],pa[1]+r],[pa[0],my],[pb[0],my],[pb[0],pb[1]-r]];}
    case 'hdirect':{                      /* neighbours: straight through the gutter */
      const sx=Math.sign(cs.dc);
      return [[pa[0]+sx*r,pa[1]],[pb[0]-sx*r,pb[1]]];}
    case 'vdirect':
      return [[pa[0],pa[1]+sy*r],[pb[0],pb[1]-sy*r]];
    case 'row':{
      /* The channel sits on the cell boundary, which IS the midpoint between the two
         rows of centres: centre(k) = ct + p/2 + k·p, so boundary k = ct + k·p. */
      const y=A.ct+cs.idx*p+off;
      if(cs.dr===0) return [[pa[0],pa[1]+r],[pa[0],y],[pb[0],y],[pb[0],pb[1]+r]];
      return [[pa[0],pa[1]+sy*r],[pa[0],y],[pb[0],y],[pb[0],pb[1]-sy*r]];}
    default:{                             /* more than one row apart: down a column channel */
      const ya=A.ct+(sy>0?A.row+1:A.row)*p, yb=B.ct+(sy>0?B.row:B.row+1)*p;
      const x=A.cl+cs.idx*p+off;
      return [[pa[0],pa[1]+sy*r],[pa[0],ya],[x,ya],[x,yb],[pb[0],yb],[pb[0],pb[1]-sy*r]];}
  }
}

/* ---------- MAP ---------- */
function drawMap(){
  const wpx=$('#svgmap').getBoundingClientRect().width||1000;
  PXK=VBW/Math.max(320,wpx);
  const L=buildLayout(), bs=L.bs, C=cone(S.focus), P=[],N=[],E=[];
  const exOnly=L.level!=='bead'&&S.agg==='ex';

  if(exOnly){
    for(const t of L.agg){
      const e=t.ref, ages=e.beads.map(i=>bs[i].age).sort((x,y)=>x-y);
      const md=ages[Math.floor(ages.length/2)]||0;
      P.push(`<rect class="agtile" data-drill="${esc(e.id)}" x="${t.x}" y="${t.y}" width="${t.w}"
        height="${t.h}" rx="2"/>`);
      P.push(`<rect class="agink" x="${t.x+.8}" y="${t.y+.8}" width="${Math.max(0,t.w-1.6)}"
        height="${Math.max(0,t.h-1.6)}" rx="1.6" fill="${heat(md)}" opacity=".45"/>`);
      const afs=px(TXT.agg);
      if(t.w>afs*7&&t.h>afs*2.6){
        const mc=Math.floor((t.w-6)/(afs*0.5));
        const lt=e.title.length>mc?e.title.slice(0,mc-1)+'…':e.title;
        P.push(`<text class="elab" x="${t.x+3}" y="${t.y+afs+1}" font-size="${afs.toFixed(2)}">${esc(lt)}</text>`);}
      if(t.w>22&&t.h>26) P.push(`<text class="agct" x="${t.x+t.w-4}" y="${t.y+t.h-4}" text-anchor="end"
        font-size="${Math.min(26,Math.max(9,t.h*.22))}" style="fill:var(--ink)" opacity=".5">${e.n}</text>`);
    }
  } else {
    for(const bd of L.bands) for(const hd of bd.heads){
      const room=Math.max(0,hd.room), fs=px(TXT.repo), fc=px(TXT.rct);
      P.push(`<line class="rsep" x1="${hd.x}" y1="${bd.y+1.5}" x2="${hd.x+Math.min(room,150)}"
        y2="${bd.y+1.5}"/>`);
      const tail=hd.cont?'cont.':String(hd.n);
      const wide=room>fs*4.5;
      const fits=Math.max(2,Math.floor(room/(fs*0.64))-(wide?tail.length+1:0));
      const nm=hd.repo.length>fits?hd.repo.slice(0,Math.max(1,fits-1))+'…':hd.repo;
      P.push(`<text class="rlab" x="${hd.x}" y="${bd.y+(bd.hdr||10)-2.5}" font-size="${fs.toFixed(2)}"${
        hd.cont?' opacity=".62"':''}>${esc(nm)}${
        wide?`<tspan class="rct" dx="5" font-size="${fc.toFixed(2)}">${tail}</tspan>`:''}</text>`);}
    for(const bl of L.blocks){
      const on=C&&bl.ref.beads.some(i=>C.all.has(i));
      P.push(`<rect class="etile${on?' on':''}" data-drill="${esc(bl.ref.id)}" x="${bl.x}" y="${bl.y}"
        width="${bl.w}" height="${bl.h}" rx="2"/>`);
      if(bl.lb>3){
        const fs=Math.min(px(TXT.epic),bl.lb*0.72);
        const mc=Math.floor((bl.w-4)/(fs*0.5));
        if(mc>=4){                       /* below four characters a title is not a title */
          const lt=bl.ref.title.length>mc?bl.ref.title.slice(0,mc-1)+'…':bl.ref.title;
          P.push(`<text class="elab" x="${bl.x+2.5}" y="${bl.y+bl.lb-2.6}"
            font-size="${fs.toFixed(2)}">${esc(lt)}</text>`);}}}
  }

  /* The live graph is 40 edges over 221 beads — that is not spaghetti, and hiding
     every edge that spans two blocks meant the dependency graph was never drawn at
     all. Cone now draws the whole graph faintly and lights the cone on focus. */
  if(S.scale==='live'&&!exOnly&&S.edges!=='off'){
    const base=Math.max(1,L.r*.155), gut=2*L.r*GAP, cr=Math.min(4.5,gut*.42);
    /* Track 1 is the channel's centreline; extra runs step out from it alternately.
       The old sequence started at -1, so a lone edge — which is nearly all of them —
       sat off-centre in its gutter rather than midway between the two rows. */
    const SEQ=[0,1,-1,2,-2], track={};
    for(const [a,b] of D.edges){
      const cs=edgeCase(a,b,L); if(!cs) continue;
      let off=0;
      if(cs.key){const n=track[cs.key]=(track[cs.key]||0)+1;
        off=SEQ[(n-1)%SEQ.length]*(gut*.26);}
      const pts=routeEdge(a,b,L,off,cs); if(!pts) continue;
      let cls='edge', sw=base;
      if(C){
        if(C.all.has(a)&&C.all.has(b)){cls='edge '+(C.up.has(a)||b===S.focus?'up':'dn'); sw=base*1.55;}
        else cls='edge faint';
      } else if(S.edges!=='all') cls='edge faint';
      E.push(`<path class="${cls}" d="${roundPath(pts,cr)}" stroke-width="${sw.toFixed(2)}"
        stroke-linecap="round" stroke-linejoin="round"/>`);}
  }

  const rr=exOnly?RFLOOR:L.r;
  if(exOnly){
    for(const t of L.agg){
      if(t.drawEx){
        for(const id of t.ref.beads){
          const p=L.posEx[id]; if(!p) continue;
          if(!inScope(id,bs)) continue;
          N.push(beadSVG(bs[id],id,p[0],p[1],rr,false,true));}
      } else if(t.ex&&t.w>70){
        const parts=[[t.exBy.rej,'var(--verm)','rejected'],[t.exBy.chn,'var(--brass)','churn'],
                     [t.exBy.poi,'var(--verm)','poisoned']].filter(p=>p[0]);
        let ly=t.y+16;
        for(const [v,c,lab] of parts){
          N.push(`<text class="agct" x="${t.x+6}" y="${ly}" font-size="11" style="fill:${c}">${v.toLocaleString()}
            <tspan font-family="IBM Plex Sans Condensed,sans-serif" font-size="8.5" dx="3"
              letter-spacing=".08em" opacity=".85">${lab.toUpperCase()}</tspan></text>`);
          ly+=13;}
      } else if(t.ex&&t.w>18){
        N.push(`<text class="agct" x="${t.x+4}" y="${t.y+15}" font-size="9"
          style="fill:var(--verm)">${t.ex.toLocaleString()}!</text>`);}}
  } else {
    for(const bl of L.blocks) for(const id of bl.ref.beads){
      const p=L.pos[id]; if(!p) continue;
      const dim=(!inScope(id,bs))||(C&&!C.all.has(id));
      N.push(beadSVG(bs[id],id,p[0],p[1],rr,dim));}
  }
  $('#svgmap').innerHTML=P.join('')+E.join('')+N.join('');

  /* readouts */
  $('#d-n').textContent=L.n.toLocaleString();
  $('#d-r').textContent=L.r.toFixed(2)+'u';
  $('#d-fill').style.width=Math.min(100,L.fill/0.4*100).toFixed(1)+'%';
  $('#d-target').style.left=Math.min(100,RHO/0.4*100).toFixed(1)+'%';
  $('#d-fillv').textContent=(L.fill*100).toFixed(0)+'% / ρ'+(RHO*100)+'%';
  $('#d-floor').textContent='~'+L.nFloor.toLocaleString();
  $('#d-floorwrap').classList.toggle('alarm',L.level!=='bead');
  $('#d-aggwrap').hidden=L.level==='bead';
  const mv=Object.values(bs).filter(moved).length;
  $('#v-moved').textContent=(mv?'▲ ':'')+mv+' moved';
  $('#v-moved').classList.toggle('zero',!mv);

  const crumb=$('#crumb');
  if(L.zoom){crumb.hidden=false;
    crumb.innerHTML=`<button id="unzoom">← everything</button><span>${esc(L.zoom.repo)}</span>`+
      (L.zoom.kind==='epic'?`<span>·</span><span>${esc(L.zoom.epic.title)}</span>`:'')+
      `<span class="mono">${L.n} beads</span>`;
    $('#unzoom').onclick=()=>{S.zoom=null;render();};
  } else crumb.hidden=true;

  const exN=Object.values(bs).filter(isException).length;
  $('#mapnote').innerHTML = L.level==='bead'
    ? `Ink covers <b>${(L.fill*100).toFixed(0)}%</b> of the stage against a ρ&nbsp;=&nbsp;${RHO*100}% target, and it
       covers about that much whatever the corpus does — blocks are sized to hold their beads at the solved
       radius rather than beads being packed into blocks sized by count.
       ${L.rFit<L.rTarget?`The stage, not the target, is binding here: nothing larger than
       <b>${L.rFit.toFixed(2)}u</b> fits.`:`The target is binding, which is the intended state.`}
       The legibility floor arrives near <b>${L.nFloor.toLocaleString()}</b> beads.`
    : `The solved radius is <b>${L.r.toFixed(2)}u</b>, under the <b>${RFLOOR}u</b> floor where a ring stops
       reading as a ring — so the stage stopped shrinking beads and started aggregating, one block per
       ${L.level}, area by count, shaded by median age.
       ${S.agg==='ex'
         ? `The ${exN.toLocaleString()} alarms are drawn on top at full size where they can still be counted by
            eye, and as counts where they cannot — a mat of alarm dots is a second bead map, not a signal.
            Click a block to drill in.`
         : `Every bead is drawn at ${L.r.toFixed(2)}u, which is what the floor exists to prevent — the density
            is right and none of the encoding survives it.`}`;
}

/* ---------- CHAINS ---------- */
function drawChains(){
  const PX=34,PY=26, CO=corpus(), CB=CO.beads;
  const all=CO.components.slice().sort((a,b)=>b.size-a.size);
  const shown=Math.min(S.chn,all.length);
  const inChains=all.reduce((s,c)=>s+c.size,0);
  $('#chainhead').innerHTML=`${all.length.toLocaleString()} components hold
    ${inChains.toLocaleString()} of ${Object.keys(CB).length.toLocaleString()} live beads.
    ${all.length>shown?`Showing the ${shown} largest — they are independent, so this is a list you
      scroll, not a graph you untangle.`:'All of them are here.'}`;
  $('#chainmore').hidden=all.length<=shown;
  $('#chainmore').textContent=`Show ${Math.min(24,all.length-shown)} more`;
  $('#chainlist').innerHTML=all.slice(0,shown).map((c,i)=>{
    const pos={}; c.nodes.forEach(n=>pos[n.id]=[22+n.l*PX,20+n.r*PY]);
    const w=22+(c.depth-1)*PX+34, h=20+(c.width-1)*PY+22;
    const serial=c.width===1&&c.size>3;
    const edges=c.edges.map(([a,b])=>{
      const A=pos[a],Z=pos[b];
      return `<path d="M${A[0]+7} ${A[1]} C${A[0]+PX/2} ${A[1]} ${Z[0]-PX/2} ${Z[1]} ${Z[0]-7} ${Z[1]}"
        fill="none" stroke="var(--line2)" stroke-width="1.3" marker-end="url(#ar)"/>`}).join('');
    const nodes=c.nodes.map(n=>{
      const b=CB[n.id],[x,y]=pos[n.id];
      /* same construction as the map: one footprint, decorations stacked inward */
      const R=8.4,T=1.35,G=.55, bands=[];
      if(moved(b))       bands.push(['var(--cyan)',.85,null]);
      if(b.rec>0)        bands.push(['var(--brass)',T,'1.8 1.4']);
      if(b.att>=THRESH)  bands.push(['var(--verm)',T,null]);
      let cur=R-T/2, deco='';
      for(const [col,w,dash] of bands){
        deco+=`<circle cx="${x}" cy="${y}" r="${cur.toFixed(2)}" fill="none" stroke="${col}"
          stroke-width="${w}"${dash?` stroke-dasharray="${dash}"`:''}/>`;
        cur-=T+G;}
      const core=bands.length?Math.max(2.6,cur+T/2+G/2):5.8;
      const f=b.poi?'var(--verm)':b.s==='in_progress'?'var(--jade)':b.s==='deferred'?'transparent':heat(b.age);
      return `<g class="bead" data-id="${n.id}">${deco}<circle cx="${x}" cy="${y}" r="${core.toFixed(2)}" fill="${f}"
        stroke="${b.s==='deferred'?heat(b.age):(n.id===S.focus?'var(--ink)':'none')}" stroke-width="2"/>
        <text x="${x}" y="${y+15}" text-anchor="middle" font-size="7"
        font-family="IBM Plex Mono,monospace" fill="var(--muted)">p${b.p}</text></g>`}).join('');
    return `<div class="card chain">
      <header><h3>Component ${i+1}</h3>
        <span class="meta">${c.size} beads · ${c.depth} deep · ${c.width} wide</span>
        ${serial?'<span class="tagser">fully serial</span>':
          c.width>1?'<span class="tagser tagpar">parallelisable</span>':''}
        <span class="meta" style="margin-left:auto">${CB[c.nodes[0].id].repo}</span></header>
      <svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">
        <defs><marker id="ar" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
          <path d="M0,0 L6,3 L0,6 Z" fill="var(--line2)"/></marker></defs>
        ${edges}${nodes}</svg>
    </div>`}).join('');
  /* The explanation is a property of the SHAPE, not of each instance of it. Repeating it
     on every serial card was already wallpaper at two components and unreadable at
     thirty — the scale test is what made that obvious. */
  const ser=all.filter(c=>c.width===1&&c.size>3);
  $('#chainnote').hidden=!ser.length;
  if(ser.length) $('#chainnote').innerHTML=`<b>${ser.length===1?'One component is':
    ser.length.toLocaleString()+' components are'} fully serial</b>, holding
    ${ser.reduce((s,c)=>s+c.size,0).toLocaleString()} beads — chained head to tail in priority order, so
    exactly one of each is ready at any moment. Nothing about the work requires that: it is the ordering
    itself encoded as blocking edges, and it caps each of those epics at one worker no matter how many
    are free.`;
}

/* ============ CHURN ============
   "How has it proceeded" is a quarter of what this surface is for and the part a
   queue cannot answer. The most valuable thing it has yet shown was a fact about
   history, and it appeared only when attempts were read as a distribution across
   the whole database rather than one bead at a time. */
function drawChurn(){
  const CB=corpus().beads, CI=Object.keys(CB);
  const stuck=CI.filter(id=>CB[id].att>0||CB[id].rec>0)
    .sort((a,b)=>(CB[b].att+CB[b].rec)-(CB[a].att+CB[a].rec));
  const CAP=28, head=stuck.slice(0,CAP), hidden=stuck.length-head.length;
  const mx=Math.max(...stuck.map(id=>CB[id].att+CB[id].rec),THRESH+1);
  const rows=head.map(id=>{const b=CB[id];
    const rw=b.att/mx*100, cw=b.rec/mx*100;
    return `<span class="id" data-id="${id}" title="${esc(b.t)}">${id}</span>
      <span class="track">
        <span class="thresh" style="left:${THRESH/mx*100}%"></span>
        ${b.att?`<i class="rej" style="width:${rw}%"></i>`:''}
        ${b.rec?`<i class="rec" style="left:${rw}%;width:${cw}%"></i>`:''}
        ${b.poi?`<span style="position:absolute;left:${rw+cw}%;top:-1px;margin-left:5px;
          font-size:10px;color:var(--verm);font-weight:600">poisoned</span>`:''}
      </span><span class="v">${b.att||'·'}/${b.rec||'·'}</span>`}).join('');
  const past=stuck.filter(id=>CB[id].att>=THRESH), unpoi=past.filter(id=>!CB[id].poi);
  const worst=stuck[0], B2=CB;

  /* the series the harness cannot yet answer for */
  const days=D.flow.days, rnd=mulberry(7);
  const series=days.map((d,i)=>({d,att:Math.round(2+rnd()*9+(i>8?i-8:0)*2.2),
    rec:Math.round(rnd()*3.4)}));
  const smx=Math.max(...series.map(s=>s.att+s.rec));
  const bars=series.map(s=>`<div style="flex:1;display:flex;flex-direction:column;justify-content:flex-end;
      align-items:center;gap:2px;min-width:0">
      <div style="width:100%;height:96px;display:flex;flex-direction:column;justify-content:flex-end;gap:1px">
        <div style="height:${s.rec/smx*100}%;background:var(--brass);opacity:.55;border-radius:2px 2px 0 0"
          title="${s.rec} reclaims"></div>
        <div style="height:${s.att/smx*100}%;background:var(--verm);border-radius:0 0 2px 2px"
          title="${s.att} attempts"></div>
      </div>
      <span class="mono" style="font-size:9px;color:var(--muted)">${s.d.slice(5)}</span></div>`).join('');

  $('#churngrid').innerHTML=`
   <div class="card panel" style="grid-column:1/-1">
     <h3>Attempts against the poison threshold <span class="prov real">live labels</span></h3>
     <p class="sub">Every bead carrying an <code style="font-family:'IBM Plex Mono',monospace">sp-attempt-N</code>
        or <code style="font-family:'IBM Plex Mono',monospace">sp-reclaim-N</code> label. Solid is the work being
        rejected; hatched is a worker dying under it. The dashed line is the threshold at ${THRESH}.</p>
     <div class="lolli">${rows}</div>
     ${hidden>0?`<div style="margin-top:8px;padding-left:92px;font-size:11.5px;color:var(--muted)">
       and <b>${hidden.toLocaleString()}</b> more with a retry history, below the ${CAP} worst. The list is
       ranked and truncated on purpose — past about thirty rows this is a table, and the shape is the
       point.</div>`:''}
     <div style="display:flex;gap:6px;align-items:center;margin-top:7px;padding-left:92px">
       <span class="threshlab">↑ threshold ${THRESH}</span>
       <span style="color:var(--muted);font-size:11px">— everything to its right should have poisoned</span></div>
     <div class="note" style="border-left-color:var(--verm)">
       <b>A bead past the threshold that has never poisoned means nothing is reading the counter.</b>
       ${past.length.toLocaleString()} beads are past ${THRESH} rejections and
       ${unpoi.length.toLocaleString()} of them have never poisoned —
       <span class="mono">${worst}</span> alone is at ${B2[worst].att}. That is not a threshold set too high;
       it is a counter being written by one mechanism and read by none, and the number it reaches is
       unbounded because nothing acts on it. Every one of these retries read as routine on the day it
       happened. The runaway is only a shape, and this panel is the only place in the surface that draws
       it.</div>
   </div>

   <div class="card panel">
     <h3>The two kinds of stuck</h3>
     <p class="sub">They demand opposite responses, and one red ring for both hides which you are looking at.</p>
     <div class="kinds">
       <svg width="34" height="34" viewBox="0 0 34 34"><circle cx="17" cy="17" r="7" fill="var(--verm)"/>
         <circle cx="17" cy="17" r="12" fill="none" stroke="var(--verm)" stroke-width="2.4"/></svg>
       <div><b>Rejected — ${CI.filter(i=>CB[i].att>=THRESH).length.toLocaleString()} beads</b>
         <p>The work keeps being refused: the gate fails, the review turns it back, the change does not hold.
            A real dead end. A person has to look at it.</p></div>
       <svg width="34" height="34" viewBox="0 0 34 34"><circle cx="17" cy="17" r="7" fill="var(--h3)"/>
         <circle cx="17" cy="17" r="12" fill="none" stroke="var(--brass)" stroke-width="2.2"
           stroke-dasharray="4 3.2"/></svg>
       <div><b>Worker churn — ${CI.filter(i=>CB[i].rec>0).length.toLocaleString()} beads</b>
         <p>The bead is fine; its workers keep dying and the lease keeps being reclaimed. An infrastructure
            signal wearing a work failure's clothes. Sending someone to read the diff is wasted.</p></div>
     </div>
     <div class="note">Watch for poison firing on the second kind. Where an attempt is charged both on
       claim and again on reclaim, one dying worker costs two of three attempts, and a bead nobody ever
       tried is removed from the queue for infrastructure flapping. The two counters being separate labels
       is what makes drawing them apart possible at all.</div>
   </div>

   <div class="card panel">
     <h3>Failures over time <span class="prov">sketch · needs an event log</span></h3>
     <p class="sub">Attempts and reclaims per day against landings. The counts on the bead are exact; their
        history is not recorded anywhere, so this shape is drawn, not measured.</p>
     <div class="hatch" style="display:flex;gap:3px;align-items:flex-end;padding:6px 4px;border-radius:5px">${bars}</div>
     <div class="note">This panel needs an append-only event log — reclaims, landings, poisonings and gate
       verdicts written as durable events, where today they live for one repaint. That log is the
       difference between noticing a runaway within the hour and noticing it when somebody happens to
       build a picture, which is how the first one here was found.</div>
   </div>`;
  $$('#churngrid .id').forEach(x=>x.onclick=()=>{S.focus=x.dataset.id;S.view='map';S.scale='live';render();});
}

/* ---------- FLOW ----------
   WHAT IS HERE AND WHAT IS DELIBERATELY NOT. Arrivals and time-in-flight are computed from
   the live beads' own timestamps and are exact. Completions per day and created-to-closed
   cycle time are not here at all: they are computed over CLOSED beads, the read path is
   bounded to work in flight, and that bound is the reason a per-request API is affordable.
   Neither is approximated from the open population — a number computed over the wrong
   population is worse than an absent one, because it reads as the number it is named after.
   The durable source for both is an append-only event log of closes and landings. */
function drawFlow(){
  const f=D.flow, mx=Math.max(...f.created,1);
  const bars=f.days.map((d,i)=>{
    const h=f.created[i]/mx*100;
    return `<div style="flex:1;display:flex;flex-direction:column;justify-content:flex-end;align-items:center;gap:2px;min-width:0">
      <div style="width:100%;display:flex;align-items:flex-end;height:118px">
        <div style="flex:1;height:${h}%;background:var(--brass);border-radius:2px 2px 0 0" title="${f.created[i]} arrived"></div>
      </div>
      <span class="mono" style="font-size:9px;color:var(--muted)">${d.slice(5)}</span></div>`}).join('');
  const fb=D.flight.buckets, fmx=Math.max(...Object.values(fb),1);
  const frow=Object.entries(fb).map(([k,v])=>{
    const col=k==='<1d'||k==='1-3d'?'var(--jade)':k==='>4w'?'var(--verm)':'var(--brass)';
    return `<span class="k">${k}</span><span class="b" style="width:${v/fmx*100}%;background:${col}"></span><span class="v">${v}</span>`}).join('');
  /* Age of the live backlog is time since anyone TOUCHED the bead; time in flight above is
     time since it was created. A bead can be hours old and already cold, or a month old and
     touched this morning, and the two panels disagreeing is the interesting case. */
  const ab={};[0,1,3,7,14,30,999].forEach((t,i,arr)=>{const lo=i?arr[i-1]:-1;
    ab[['0d','1d','2-3d','4-7d','1-2w','2-4w','30d+'][i]]=IDS.filter(id=>B[id].age>lo&&B[id].age<=t).length;});
  const amx=Math.max(...Object.values(ab),1);
  const arow=Object.entries(ab).map(([k,v],i)=>
    `<span class="k">${k}</span><span class="b" style="width:${v/amx*100}%;background:var(${HEAT[i]})"></span><span class="v">${v}</span>`).join('');
  const lanes=D.repos.map(r=>{
    const cc=f.byrepo[r.repo]||f.days.map(()=>0), tot=cc.reduce((a,b)=>a+b,0), m=Math.max(...cc,1);
    const sp=cc.map((v,i)=>`${i/Math.max(cc.length-1,1)*100},${28-v/m*24}`).join(' ');
    return `<span>${esc(r.repo)}</span><span class="ct">${r.n}</span><span class="ct">${tot}</span>
      <svg viewBox="0 0 100 30" preserveAspectRatio="none" style="width:100%;height:22px">
        <polyline points="${sp}" fill="none" stroke="var(--brass)" stroke-width="1.6" vector-effect="non-scaling-stroke"/></svg>`}).join('');
  const mvIds=IDS.filter(id=>moved(B[id]));
  const arrived=f.created.reduce((a,b)=>a+b,0);
  $('#flowgrid').innerHTML=`
   <div class="card panel" style="grid-column:1/-1"><h3>What moved</h3>
     <p class="sub">${mvIds.length} beads were touched in the selected window. A refresh that renders an
       identical picture is wallpaper by the second day — this is the part of the page that must not be.</p>
     <div class="movedlist">${mvIds.length?mvIds.map(id=>
       `<button data-id="${id}" title="${esc(B[id].t)}">${id} <span style="color:var(--muted)">${B[id].s}</span></button>`).join('')
       :'<span style="color:var(--muted);font-size:11.5px">Nothing moved in this window.</span>'}</div></div>
   <div class="card panel"><h3>Arrivals</h3>
     <p class="sub">${arrived} of the ${st.live} beads now in flight were filed in the last ${f.days.length} days.</p>
     <div style="display:flex;gap:3px;align-items:flex-end">${bars}</div>
     <div class="note">Arrivals only. Completions are counted over closed beads and no closed bead is
       fetched — the query is bounded to work in flight, which is what makes reading it on every
       request affordable. Putting a completions bar here would mean deriving it from the open
       population, where it would be wrong and would still read as the number it is named after.</div></div>
   <div class="card panel"><h3>Time in flight</h3>
     <p class="sub">How long the work now open has been open — created to now, over ${st.live} beads.</p>
     <div class="hist">${frow}</div>
     <div class="note">This is not cycle time. Every bead counted here is still open, so the
       distribution is censored on the right and each bucket is a lower bound; the beads that would
       move it most are the ones still in the last bucket. Created-to-closed needs the closed
       population and belongs to the event log.</div></div>
   <div class="card panel"><h3>Age of the live backlog</h3>
     <p class="sub">Time since anyone last touched the bead.</p>
     <div class="hist">${arow}</div>
     <div class="note">${st.cold} of ${st.live} live beads have gone untouched for more than two weeks.</div></div>
   <div class="card panel"><h3>By repository</h3>
     <p class="sub">Live now, arrivals in the window, and the arrival trend.</p>
     <div class="lanes"><span class="lab">repo</span><span class="lab">live</span><span class="lab">new</span>
       <span class="lab">trend</span>${lanes}</div></div>`;
  $$('#flowgrid .movedlist button').forEach(x=>x.onclick=()=>{S.focus=x.dataset.id;S.view='map';S.scale='live';render();});
}

/* ---------- inspector ---------- */
function drawInsp(){
  const app=$('#app');
  if(!S.focus||!B[S.focus]){app.classList.remove('insp');$('#insp').innerHTML='';return;}
  app.classList.add('insp');
  const b=B[S.focus],C=cone(S.focus);
  const sc=b.poi?['var(--verm)','poisoned']:{in_progress:['var(--jade)','moving'],deferred:['var(--brass)','deferred'],
    blocked:['var(--verm)','blocked'],open:['var(--cyan)','ready'],hooked:['var(--violet)','hooked']}[b.s]||['var(--muted)',b.s];
  const row=id=>{const t=B[id];return `<button data-id="${id}">
    <span style="width:7px;height:7px;border-radius:50%;background:${heat(t.age)};flex:none"></span>
    <span class="mono" style="color:var(--muted);flex:none">${id}</span>
    <span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(t.t)}</span></button>`};
  const stuckNote=
    b.att>=THRESH&&b.rec>0?`<div class="note" style="border-left-color:var(--verm)">Both kinds at once:
      ${b.att} rejections and ${b.rec} reclaims. Read the reclaims first — if the workers keep dying, the
      rejection count is partly an artefact of that and the diff may be fine.</div>`
    :b.att>=THRESH?`<div class="note" style="border-left-color:var(--verm)"><b>Rejected ${b.att} times.</b>
      The work itself keeps being refused; this is a dead end and needs a person, not another attempt.
      ${b.poi?'The harness has stopped retrying.':`It has passed the threshold of ${THRESH} and has <b>not</b> poisoned — the counter and the threshold are out of step here.`}</div>`
    :b.rec>0?`<div class="note" style="border-left-color:var(--brass)"><b>${b.rec} reclaims.</b> Workers keep
      dying under this bead and the lease keeps being taken back. That is an infrastructure signal — the bead
      is probably fine, and reading the diff will not find anything.</div>`:'';
  $('#insp').innerHTML=`
    <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
      <span class="mono" style="font-size:13px;font-weight:600">${S.focus}</span>
      <span class="pill" style="background:color-mix(in srgb,${sc[0]} 16%,transparent);color:${sc[0]}">${sc[1]}</span>
      ${moved(b)?'<span class="dchip">moved</span>':''}
      <button style="margin-left:auto;color:var(--muted);font-size:16px;line-height:1" id="close" aria-label="Close">×</button></div>
    <h3 style="margin:9px 0 0;font-size:14px;font-weight:500;line-height:1.35">${esc(b.t)}</h3>
    <dl class="kv">
      <dt>repo</dt><dd>${b.repo}</dd>
      <dt>epic</dt><dd>${b.ep?esc((B[b.ep]||{t:b.ep}).t):'—'}</dd>
      <dt>priority</dt><dd class="mono">p${b.p} · ${b.ty}</dd>
      <dt>created</dt><dd>${b.born}d ago</dd>
      <dt>touched</dt><dd style="color:${heat(b.age)}">${b.age}d ago</dd>
      <dt>assignee</dt><dd>${b.asg||'—'}</dd>
      <dt>layer</dt><dd class="mono">${b.lay} · ${C.up.size} upstream, ${C.dn.size} downstream</dd>
      <dt>rejected</dt><dd class="mono" style="color:${b.att>=THRESH?'var(--verm)':'var(--ink2)'}">${b.att} / ${THRESH}</dd>
      <dt>reclaimed</dt><dd class="mono" style="color:${b.rec?'var(--brass)':'var(--ink2)'}">${b.rec}</dd>
    </dl>
    ${stuckNote}
    ${b.d?`<div class="lab" style="margin-bottom:5px">Description</div><div class="desc">${esc(b.d)}</div>`:''}
    ${C.up.size?`<div class="lab" style="margin:14px 0 0">Blocked by</div>
      <div class="conelist">${[...C.up].map(row).join('')}</div>`:''}
    ${C.dn.size?`<div class="lab" style="margin:14px 0 0">Unblocks</div>
      <div class="conelist">${[...C.dn].map(row).join('')}</div>`:''}
    ${!C.up.size&&!C.dn.size?`<div class="note" style="margin-top:14px">Nothing blocks this and it blocks nothing — it can be worked the moment someone picks it up.</div>`:''}`;
  $('#close').onclick=()=>{S.focus=null;render();};
  $$('#insp .conelist button').forEach(x=>x.onclick=()=>{S.focus=x.dataset.id;render();});
}

/* ---------- ops pane ---------- */
let OPS=null, opsTimer=null;
const OPS_API=API.replace(/beads(\?.*)?$/, 'ops');

function fetchOps(){
  fetch(OPS_API,{headers:{accept:'application/json'},cache:'no-store'})
    .then(r=>r.json())
    .then(data=>{OPS=data; if(S.view==='ops') drawOps();})
    .catch(e=>console.warn('ops fetch failed',e));
}

function drawOps(){
  const el=document.getElementById('ops-out');
  if(!OPS){el.innerHTML='<div style="color:var(--muted);padding:16px 0">Loading…</div>';return;}
  const d=OPS;
  const q=k=>Object.prototype.hasOwnProperty.call(d,k)?d[k]:null;
  const sq=k=>{const v=q(k);return v!==null?String(v):null;};
  const E=s=>String(s==null?'':s).replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));
  const ok=v=>`<span class="o-ok">${E(v)}</span>`;
  const bad=v=>`<span class="o-bad">${E(v)}</span>`;
  const warn=v=>`<span class="o-warn">${E(v)}</span>`;
  const dim=v=>`<span style="color:var(--muted)">${E(v)}</span>`;
  const bold=v=>`<b>${E(v)}</b>`;
  const QM=()=>bad('?');
  const str=k=>{const v=q(k);return v!==null?E(String(v)):QM();};
  function numC(v,t,unit){
    if(v===null||v===undefined)return QM()+(unit?E(unit):'');
    const n=parseInt(v,10);if(isNaN(n))return QM()+(unit?E(unit):'');
    const s=String(v)+(unit||'');return n>=t?warn(s):ok(s);
  }
  function badZ(v){
    if(v===null||v===undefined)return QM();
    return String(v)==='0'?ok('0'):bad(String(v));
  }
  function tok(v){
    if(v===null||v===undefined)return '?';
    const n=parseInt(v,10);if(isNaN(n))return String(v);
    if(n>=1000000)return Math.floor(n/1000000)+'M';
    if(n>=1000)return Math.floor(n/1000)+'k';
    return String(n);
  }
  function ageS(secs,warnAt){
    if(secs===null||secs===undefined)return QM();
    const n=parseInt(secs,10);if(isNaN(n))return QM();
    const s=n<120?n+'s':Math.floor(n/60)+'m';
    return n>=warnAt?bad(s):dim(s);
  }
  function pctC(p){
    if(p===null)return QM();const n=parseInt(p,10);if(isNaN(n))return QM();
    if(n>=90)return bad(p+'%');if(n>=70)return warn(p+'%');return ok(p+'%');
  }
  function durMin(m){
    if(m===null)return '?';const n=parseInt(m,10);if(isNaN(n))return '?';
    if(n===0)return '0m';if(n<60)return n+'m';
    if(n<1440)return Math.floor(n/60)+'h'+(n%60?n%60+'m':'');
    return Math.floor(n/1440)+'d'+(Math.floor(n%1440/60)?Math.floor(n%1440/60)+'h':'');
  }
  const row=(l,c)=>`<div class="orow"><span class="olab">${E(l)}</span><span class="oval">${c}</span></div>`;
  const sub=c=>`<div class="osub">${c}</div>`;
  const sep=()=>'<div class="osep"></div>';

  let h='<div class="obox">';

  /* HALT/DRAIN banners — read from stamps, not from the snapshot, so a dead collector
     does not render a halted world as running. */
  if(d.halted){
    let b=`<div class="obanner halt">■ SPIRA STOPPED — no aeons are summoned; nothing lands`;
    if(d.halted_since)b+=`<br><span style="font-weight:normal">${dim('since')} ${E(d.halted_since)}</span>`;
    if(d.halted_why)b+=`<br><span style="font-weight:normal">${dim('why')} ${E(d.halted_why)}</span>`;
    h+=b+'</div>';
  } else if(d.sentinel_active===false){
    h+='<div class="obanner halt">■ SPIRA STOPPED — sentinel.timer is not active and no halt stamp was recorded</div>';
  }
  if(d.draining){
    const mins=d.draining_age_s!==null?Math.floor(d.draining_age_s/60):'?';
    h+=`<div class="obanner drain">⏸ DRAINING — summons gated for ${E(String(mins))}m — loop and landing continue</div>`;
  }

  /* Header — SPIRA HH:MM  sentinel/ops/auron dots  STALE if snapshot is > 3 min old */
  const sentDot=d.sentinel_active===true?ok('●'):d.sentinel_active===null?QM():bad('○');
  const opsDot=(s=>s==='1'?ok('●'):s==='0'?bad('○'):QM())(sq('SP_OPS_TIMER'));
  const auronDot=(s=>s==='1'?ok('●'):s==='0'?bad('○'):QM())(sq('SP_AURON_TIMER'));
  let staleH='';
  const atE=q('SP_AT');
  if(atE!==null){const ag=Math.floor(Date.now()/1000)-parseInt(atE,10);if(!isNaN(ag)&&ag>180)staleH='  '+bad(`STALE ${ag}s`);}
  h+=row('SPIRA',
    new Date().toTimeString().slice(0,5)+'  '+
    dim('sentinel')+' '+sentDot+' '+ageS(q('SP_SENTINEL_AGE'),180)+'  '+
    dim('ops')+' '+opsDot+' '+ageS(q('SP_OPS_AGE'),180)+staleH);
  h+=sub(dim('auron')+' '+auronDot+' '+ageS(q('SP_AURON_AGE'),180));
  const af=q('SP_AURON_FIRING');
  if(af!==null&&af!=='?'&&String(af)!=='0')
    h+=sub(bad('ALERT')+' '+bad(String(af))+' firing  '+dim(E(sq('SP_AURON_KEYS')||'')));

  h+=sep();

  /* TOKENS / CTX / WIN */
  const tw=q('SP_TOK_WIN'),twh=sq('SP_TOK_WINDOW_H')||'?';
  h+=row(`TOKENS/${twh}h`, bold(tok(tw))+' billed');
  for(const[k,name]of[['AEON','aeons'],['ARC','archivist'],['SESS','session']]){
    const w=q(`SP_TOK_${k}_WIN`),t=q(`SP_TOK_${k}_TURNS`),c=q(`SP_TOK_${k}_CTX`);
    const pct=(w!==null&&tw&&parseInt(tw,10)>0)?Math.round(parseInt(w,10)*100/parseInt(tw,10))+'%':'?';
    h+=sub(`${E(name)} ${pct}  ${t!==null?E(String(t)):'?'}t · ${tok(c)} ctx/t`);
  }
  const ctxN=q('SP_CTX_NOW');
  if(ctxN==='-'){h+=sub(dim('no session at the keyboard'));}
  else if(ctxN!==null&&ctxN!=='?'&&/^\d+$/.test(String(ctxN))){
    let cx=dim('ctx')+' '+bold(tok(ctxN));
    const ct=q('SP_CTX_TURNS');if(ct!==null)cx+=` / ${E(String(ct))}t`;
    const cn=q('SP_CTX_NEXT');
    if(cn==='warn'||cn==='high')cx+='  '+warn(cn);
    else if(cn==='limit'||cn==='over')cx+='  '+bad(cn);
    h+=sub(cx);
  }
  const p5=q('SP_RATELIM_5H_PCT'),p7=q('SP_RATELIM_7D_PCT');
  const m5=q('SP_RATELIM_5H_MIN'),m7=q('SP_RATELIM_7D_MIN');
  h+=row('WIN',
    '5h '+pctC(p5)+'  '+dim('reset')+' '+E(durMin(m5))+'  '+
    '7d '+pctC(p7)+'  '+dim('reset')+' '+E(durMin(m7)));

  h+=sep();

  /* NOW — working aeons */
  const an=q('SP_AEON_N');
  if(an===null||an==='?'){h+=row('NOW',QM()+dim(' cannot read the aeon roster'));}
  else if(parseInt(an,10)===0){h+=row('NOW',dim('no aeon working'));}
  else{
    const nn=parseInt(an,10);
    for(let i=0;i<Math.min(nn,5);i++){
      const nm=sq(`SP_AEON${i}_NAME`)||'?',fy=sq(`SP_AEON${i}_FAYTH`)||'?',bd2=sq(`SP_AEON${i}_BEAD`)||'?';
      h+=row(i===0?'NOW':'', bold(nm)+'  '+dim(fy)+'  '+E(bd2));
    }
    if(nn>5)h+=sub(dim(`…${nn-5} more`));
  }

  /* NEXT */
  const nxN=q('SP_NEXT_N');
  if(nxN===null||nxN==='?'){h+=row('NEXT',QM()+dim(' cannot read the ready queue'));}
  else if(parseInt(nxN,10)===0){h+=row('NEXT',dim('0 ready — nothing to claim'));}
  else{
    h+=row('NEXT',bold(E(String(nxN)))+dim(' ready'));
    for(let i=0;i<5;i++){const r=sq(`SP_NEXT${i}`);if(!r)break;h+=sub(E(r));}
  }

  /* UNLND */
  const pN=q('SP_PEND_N');
  if(pN===null||pN==='?'){h+=row('UNLND',QM()+dim(' cannot read the unlanded queue'));}
  else if(parseInt(pN,10)===0){h+=row('UNLND',dim('nothing waiting to land'));}
  else{
    const pO=sq('SP_PEND_OLDEST')||'?';
    h+=row('UNLND',bold(E(String(pN)))+dim(' closed, not on base — oldest ')+
      (/[hd]/.test(pO)?warn(pO):dim(pO)));
    for(let i=0;i<5;i++){const r=sq(`SP_PEND${i}`);if(!r)break;h+=sub(E(r));}
  }

  /* RECENT */
  const e0=sq('SP_EVENT0');
  if(!e0){h+=row('RECENT',d.cockpit_env_error?QM()+dim(' no snapshot to read'):dim('nothing in the window'));}
  else{
    for(let i=0;i<8;i++){const ev=sq(`SP_EVENT${i}`);if(!ev)break;h+=row(i===0?'RECENT':'',E(ev));}
  }

  /* CI */
  const caN=q('SP_AWAITING_N'),caSt=q('SP_AWAITING_STUCK');
  if(caN===null||caN==='?'){h+=row('CI',QM()+dim(' cannot read what is parked on CI'));}
  else if(parseInt(caN,10)===0&&(!caSt||String(caSt)==='0')){h+=row('CI',dim('nothing parked on CI'));}
  else{
    const caA=sq('SP_AWAITING_AGE')||'?',caO=sq('SP_AWAITING_OLDEST')||'?';
    let ci=bold(E(String(caN)))+dim(' bead(s) waiting on a run  oldest ')+E(caO)+' '+(/[hd]/.test(caA)?warn(caA):dim(caA));
    if(caSt&&String(caSt)!=='0')
      ci+='  '+warn(E(String(caSt))+' parked with no run')+dim(` (${E(sq('SP_AWAITING_STUCK_ID')||'?')})`);
    h+=row('CI',ci);
    for(let i=0;i<5;i++){const r=sq(`SP_AWAITING${i}`);if(!r)break;h+=sub(E(r));}
  }

  h+=sep();

  /* ATTN */
  const ua=q('SP_UNANSWERED');
  h+=row('ATTN',
    dim('waiting on you')+' '+bold(str('SP_WAITING'))+'  '+
    dim('threads')+' '+(ua!==null&&String(ua)!=='0'?bad(String(ua)):ok(ua!==null?String(ua):'?')));

  /* SEND */
  const sf=q('SP_SENT_FAILED'),bd3=q('SP_BRANCH_DONE');
  h+=row('SEND',
    bold(str('SP_UNSENT'))+dim(' branches unsent')+'  '+
    (bd3!==null&&String(bd3)!=='0'?warn(String(bd3)):str('SP_BRANCH_DONE'))+dim(' awaiting rites')+'  '+
    dim('oldest ')+str('SP_UNSENT_OLDEST_H')+dim('h'));
  h+=sub((sf!==null&&String(sf)!=='0'?bad(String(sf)):ok(sf!==null?String(sf):'?'))+dim(' fiends — unsent work that came back'));

  /* BEADS */
  h+=row('BEADS',dim('24h')+'  closed '+str('SP_CLOSED_24H')+'  '+dim('opened')+' '+str('SP_OPENED_24H'));
  h+=sub(dim('opened ')+str('SP_BEADS_SPARK_OPENED')+'  '+dim('closed ')+str('SP_BEADS_SPARK_CLOSED'));
  h+=sub(dim('landed ')+str('SP_BEADS_SPARK_LANDED')+'  '+str('SP_BEADS_LANDED_24H')+dim(' in 24h'));
  const uld=q('SP_UNLANDED');
  h+=sub(dim('24h worked ')+str('SP_CLOSED')+dim(': ')+str('SP_LANDED')+dim(' landed')+'  '+
    dim(str('SP_AWAITING_LAND')+' awaiting')+'  '+
    (uld!==null&&String(uld)!=='0'?bad(String(uld)):ok(uld!==null?String(uld):'?'))+dim(' never landed'));

  /* LAND */
  const lAt=q('SP_LAND_AT');
  const lAge=lAt!==null&&/^\d+$/.test(String(lAt))?Math.floor(Date.now()/1000)-parseInt(lAt,10):null;
  const lRc=q('SP_LAND_RC');
  const lRcH=lRc===null||lRc==='?'?QM():String(lRc)==='0'?ok('0'):warn(String(lRc));
  h+=row('LAND',
    dim('last')+' '+ageS(lAge,600)+'  '+dim('rc')+' '+lRcH+'  '+
    dim('branches')+' '+str('SP_LAND_BRANCHES')+'  '+dim('moved')+' '+str('SP_LAND_MOVED'));
  const glv=q('SP_GATE_LIVE');
  if(glv&&String(glv)!=='0'&&String(glv)!=='?'){
    h+=sub(bold(E(String(glv)))+dim(' gate(s) running'));
    const gN=q('SP_GATE_N')||0;
    for(let gi=0;gi<Math.min(parseInt(gN,10),5);gi++){
      const gs=sq(`SP_GATE${gi}_SLUG`)||'?',ga=q(`SP_GATE${gi}_AGE`),gp=sq(`SP_GATE${gi}_PHASE`)||'?';
      const gas=ga!==null?(parseInt(ga,10)<120?parseInt(ga,10)+'s':Math.floor(parseInt(ga,10)/60)+'m'):'?';
      h+=sub(E(gs)+'  '+(gp==='waiting'?warn(gp):dim(gp))+'  '+dim(gas));
    }
  }
  const lpN=q('SP_LANDPROG_N');
  if(lpN&&String(lpN)!=='0'&&String(lpN)!=='?')
    for(let i=0;i<Math.min(parseInt(lpN,10),5);i++){const r=sq(`SP_LANDPROG${i}`);if(r)h+=sub(E(r));}

  /* GATE */
  const yF=q('SP_YIELD_FAULT');
  const yFH=yF===null||yF==='?'?QM():String(yF)==='0'?ok('0'):warn(String(yF));
  const ySolo=q('SP_YIELD_SOLO_MED'),yConc=q('SP_YIELD_CONC_MED');
  h+=row('GATE',
    dim('reds')+' '+str('SP_YIELD_REDS')+'  '+dim('defect')+' '+str('SP_YIELD_DEFECT')+'  '+
    dim('fault')+' '+yFH+'  '+dim('unknown')+' '+str('SP_YIELD_UNKNOWN'));
  h+=sub(dim('cost ')+
    (ySolo!==null&&ySolo!=='?'?E(String(ySolo))+'s':'?')+dim(' solo · ')+
    (yConc!==null&&yConc!=='?'?E(String(yConc))+'s':'?')+dim(' with another (median)'));

  /* SELF — repeating alerts, birth, stall, judgement */
  const srN=q('SP_SELF_REPEATING_N')||0;
  let selfSeen=false;
  for(let si=0;si<Math.min(parseInt(srN,10),3);si++){
    const t=sq(`SP_SELF_REPEATING${si}`);
    if(t){h+=row(si===0?'SELF':'',bad('REPEATING')+'  '+dim(E(t)));selfSeen=true;}
  }
  const sb=q('SP_SELF_STILLBORN_W');
  if(sb!==null&&sb!=='?'&&parseInt(sb,10)>0)
    h+=row('BIRTH',bad(String(sb))+dim(' died at birth  · last ')+str('SP_SELF_STILLBORN_LAST'));
  const sv=q('SP_SELF_STARVED_W');
  if(sv!==null&&sv!=='?'&&parseInt(sv,10)>0)
    h+=row('STALL',bad(String(sv))+dim(' stalled passes  · last ')+str('SP_SELF_STARVED_LAST'));
  const jud=q('SP_SINCE_JUDGEMENT');
  const judH=jud===null||jud==='?'?QM():String(jud)==='n/a'?dim('not needed'):
    String(jud).startsWith('NEVER')?bad(jud):dim(E(String(jud))+' passes ago');
  h+=row('SELF',dim('judgement')+' '+judH);

  /* CAPACITY — the account's own five-hour window. Rendered only while paused: a row that
     is always present but usually says nothing is a row nobody reads. */
  if(String(q('SP_CAPACITY_PAUSED'))==='1'){
    const cl=q('SP_CAPACITY_LEFT'),cm=cl!==null?Math.floor(parseInt(cl,10)/60):'?';
    h+=row('CAPACITY',bad('ACCOUNT OUT OF CAPACITY')+dim(' until ')+str('SP_CAPACITY_AT')+
      dim(` (${E(String(cm))}m)  summoning paused`));
  }

  /* OPS */
  h+=row('OPS',dim('never-fired ')+numC(q('SP_SOP_NEVER_FIRED'),5)+'  '+dim('recurred (no hold) ')+badZ(q('SP_SOP_RECURRED')));

  /* File freshness footer */
  h+=sep();
  const ceA=q('cockpit_env_age_s'),ceE=q('cockpit_env_error');
  h+=sub(dim('cockpit.env ')+(ceE?bad('error: '+E(String(ceE))):ageS(ceA,300)));

  h+='</div>';
  el.innerHTML=h;
}

/* ---------- render / events ---------- */
function render(){
  $$('.tab').forEach(t=>t.setAttribute('aria-selected',t.dataset.view===S.view));
  $$('[data-pane]').forEach(p=>p.classList.toggle('hidden',p.dataset.pane!==S.view));
  $$('.rrow').forEach(r=>r.setAttribute('aria-pressed',r.dataset.repo===S.repo));
  $$('.sv').forEach(r=>r.setAttribute('aria-pressed',+r.dataset.saved===S.saved));
  $$('[data-edges]').forEach(b=>b.setAttribute('aria-pressed',b.dataset.edges===S.edges));
  $$('[data-scale]').forEach(b=>b.setAttribute('aria-pressed',b.dataset.scale===S.scale));
  $$('[data-since]').forEach(b=>b.setAttribute('aria-pressed',+b.dataset.since===S.since));
  $$('[data-agg]').forEach(b=>b.setAttribute('aria-pressed',b.dataset.agg===S.agg));
  if(S.view==='map')drawMap(); if(S.view==='chains')drawChains();
  if(S.view==='churn')drawChurn(); if(S.view==='flow')drawFlow();
  if(S.view==='ops'){
    drawOps();
    if(!opsTimer){fetchOps();opsTimer=setInterval(fetchOps,10000);}
  } else {
    if(opsTimer){clearInterval(opsTimer);opsTimer=null;}
  }
  if(S.view!=='map'){const mv=IDS.filter(id=>moved(B[id])).length;
    $('#v-moved').textContent=(mv?'▲ ':'')+mv+' moved'; $('#v-moved').classList.toggle('zero',!mv);}
  drawInsp();
  history.replaceState(null,'','#'+new URLSearchParams(Object.fromEntries(
    Object.entries(S).filter(([k,v])=>v!==null&&v!==undefined))).toString());
}
$$('.tab').forEach(t=>t.onclick=()=>{S.view=t.dataset.view;render();});
$$('.rrow').forEach(r=>r.onclick=()=>{S.repo=S.repo===r.dataset.repo?null:r.dataset.repo;render();});
$$('.sv').forEach(r=>r.onclick=()=>{S.saved=S.saved===+r.dataset.saved?null:+r.dataset.saved;render();});
$$('[data-edges]').forEach(b=>b.onclick=()=>{S.edges=b.dataset.edges;render();});
$$('[data-scale]').forEach(b=>b.onclick=()=>{S.scale=b.dataset.scale;S.zoom=null;S.focus=null;S.chn=12;render();});
$('#chainmore').onclick=()=>{S.chn+=24;render();};
$$('[data-since]').forEach(b=>b.onclick=()=>{S.since=+b.dataset.since;render();});
$$('[data-agg]').forEach(b=>b.onclick=()=>{S.agg=b.dataset.agg;render();});
document.addEventListener('click',e=>{
  const ag=e.target.closest('[data-drill]');
  if(ag&&(e.target.classList.contains('agtile')||e.target.classList.contains('etile'))){
    S.zoom=S.zoom===ag.dataset.drill?null:ag.dataset.drill;render();return;}
  const n=e.target.closest('.bead'); if(!n)return;
  if(!B[n.dataset.id]) return;                    /* synthetic beads have no record to open */
  S.focus=S.focus===n.dataset.id?null:n.dataset.id; render();});
const tip=$('#tip');
document.addEventListener('mouseover',e=>{
  const n=e.target.closest('.bead'); if(!n){tip.style.display='none';return;}
  const b=(corpus().beads)[n.dataset.id]||B[n.dataset.id]; if(!b){tip.style.display='none';return;}
  const stuck=b.att>=THRESH?`REJECTED ${b.att}×`:b.rec>0?`worker churn ${b.rec}×`:b.s;
  tip.innerHTML=`<b>${esc(b.t)}</b><span>${n.dataset.id} · ${b.repo} · p${b.p} · ${stuck} · touched ${b.age}d ago</span>`;
  tip.style.display='block';});
document.addEventListener('mousemove',e=>{
  if(tip.style.display!=='block')return;
  const r=tip.getBoundingClientRect();
  tip.style.left=Math.min(e.clientX+14,innerWidth-r.width-10)+'px';
  tip.style.top=Math.min(e.clientY+16,innerHeight-r.height-10)+'px';});
document.addEventListener('keydown',e=>{
  if(e.key==='Escape'){if(S.zoom)S.zoom=null;else S.focus=null;render();}
  const V=['map','chains','churn','flow','build','ops'];
  if(e.key>='1'&&e.key<='6'){S.view=V[+e.key-1];render();}});

/* AUTO-REFRESH — re-fetch when the server's cache window closes so a tab left open
   shows what changed rather than what was. The server reports `cache_s` (how long it
   holds its snapshot) and `age_ms` (how old the snapshot is when the response arrives).
   We wait until that window has closed — cache_s seconds minus the elapsed age, plus
   2 s of headroom — and then fetch again. One query per cache window regardless of how
   many tabs are open, because the server's own cache bounds the cost.
   The re-fetch intentionally waits for the server cache to expire first; fetching early
   would return the same snapshot and burn a round trip for nothing. */
var _refreshTimer=null;
function scheduleRefresh(cacheS,ageMs){
  if(_refreshTimer) clearTimeout(_refreshTimer);
  /* Floor at 5 s to stay clear of a tight loop if the server reports an unusual age. */
  var delay=Math.max(5000,(cacheS||15)*1000-(ageMs||0)+2000);
  _refreshTimer=setTimeout(doRefresh,delay);
}
function doRefresh(){
  var t0=(typeof performance!=='undefined'?performance:Date).now();
  fetch(API,{headers:{accept:'application/json'},cache:'no-store'})
    .then(function(r){
      if(!r.ok) throw new Error('HTTP '+r.status);
      return r.json();
    })
    .then(function(payload){
      var read=(typeof performance!=='undefined'?performance:Date).now()-t0;
      var p=LoomModel.unwrap(payload);
      var t1=(typeof performance!=='undefined'?performance:Date).now();
      var model=LoomModel.derive(p.beads,Object.assign({},p.meta,{
        edges:p.edges,
        now:p.meter.generatedAtMs||undefined
      }));
      var derive=(typeof performance!=='undefined'?performance:Date).now()-t1;
      /* Swap in the new corpus. synCache is keyed to the old bead set — discard it. */
      D=model; B=D.beads; IDS=Object.keys(B); st=D.stats;
      REAL={beads:B,components:D.components,repos:D.repos.map(r=>({repo:r.repo,n:r.n,
        epics:r.epics.map(e=>({id:e.id,title:e.title,n:e.n,beads:e.beads}))}))};
      synCache={};
      paintBar({beads:p.beads.length,read:read,derive:derive,meter:p.meter});
      render();
      scheduleRefresh(p.meter.cacheS,p.meter.ageMs);
    })
    .catch(function(){
      /* On error, retry after one full cache window. */
      scheduleRefresh(timing.meter&&timing.meter.cacheS,0);
    });
}
scheduleRefresh(timing.meter&&timing.meter.cacheS,timing.meter&&timing.meter.ageMs);
render();

}
