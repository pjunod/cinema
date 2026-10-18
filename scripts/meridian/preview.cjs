#!/usr/bin/env node
"use strict";
// M0 design evidence only. It neither registers Meridian nor runs a native app.
// PLAYWRIGHT_MODULE/SHARP_MODULE can select existing installed packages.
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),cp=require('node:child_process'),assert=require('node:assert/strict'),crypto=require('node:crypto');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const sharp=require(process.env.SHARP_MODULE||'sharp');
const root=path.resolve(__dirname,'../..'),out=path.join(root,'target/meridian/m0');
const read=p=>fs.readFileSync(path.join(root,p),'utf8');
const candidate=JSON.parse(read('scripts/meridian/tokens.json'));
const themeSource=read('crates/plurxd/src/web/core/theme.js');
const themes=vm.runInNewContext(themeSource.slice(0,themeSource.indexOf('// Favicons:'))+';THEMES');
Object.assign(themes,candidate);
const catalog=read('crates/plurxd/src/web/layouts/catalog.js');
const icons=vm.runInNewContext(catalog.slice(catalog.indexOf('const CATALOG_GLYPHS'),catalog.indexOf('// Both spellings'))+';Object.fromEntries(Object.keys(CATALOG_GLYPHS).map(k=>[k,catalogIcon(k)]))');
const template=read('scripts/meridian/preview.html');
const source=cp.execFileSync('git',['rev-parse','HEAD'],{cwd:root,encoding:'utf8'}).trim();
const rendererHash=crypto.createHash('sha256').update(template).update(read('scripts/meridian/preview.cjs')).update(read('scripts/meridian/tokens.json')).digest('hex');
const rgb=h=>h.match(/[0-9a-f]{2}/gi).map(v=>parseInt(v,16));
const lum=v=>v.map(c=>c/255).map(c=>c<=.04045?c/12.92:((c+.055)/1.055)**2.4).reduce((s,c,i)=>s+c*[.2126,.7152,.0722][i],0);
const contrast=(a,b)=>(Math.max(lum(a),lum(b))+.05)/(Math.min(lum(a),lum(b))+.05);
const ratio=(a,b)=>contrast(rgb(a),rgb(b));
const audit=[];
function check(label,value,threshold){audit.push({label,ratio:Number(value.toFixed(3)),threshold,pass:value>=threshold});assert.ok(value>=threshold,`${label}: ${value.toFixed(2)} < ${threshold}`);}
for(const mode of ['dark','light']){
 const t=candidate.meridian[mode];
 for(const foreground of ['--text','--muted','--prose','--accent','--accent2','--good','--bad','--warn'])for(const background of ['--bg','--panel','--panel2'])check(`meridian/${mode} ${foreground} on ${background}`,ratio(t[foreground],t[background]),4.5);
 check(`meridian/${mode} action ink`,ratio(t['--btn-ink'],t['--accent']),4.5);
 check(`meridian/${mode} selected ink`,ratio(t['--selected-ink'],t['--selected-bg']),4.5);
 check(`meridian/${mode} focus inner on selected`,ratio(t['--bg'],t['--selected-bg']),3);
 check(`meridian/${mode} focus inner on action`,ratio(t['--bg'],t['--accent']),3);
 for(const bg of ['--bg','--panel','--panel2'])check(`meridian/${mode} focus outer on ${bg}`,ratio(t['--accent'],t[bg]),3);
 for(const extreme of [0,255])for(const fg of ['--text','--muted']){
  const composite=rgb(t['--bg']).map(c=>c*.94+extreme*.06);
  check(`meridian/${mode} hero 94% ${fg} over ${extreme}`,contrast(rgb(t[fg]),composite),4.5);
 }
}
const specs=[
 ['desktop-home-dark',1440,1080,'desktop','home','dark'],
 ['desktop-home-light',1440,1080,'desktop','home','light'],
 ['desktop-library',1440,1080,'desktop','library','dark'],
 ['desktop-detail',1440,1080,'desktop','detail','dark'],
 ['phone-home-dark',390,1050,'phone','home','dark'],
 ['phone-home-light',390,1050,'phone','home','light'],
 ['phone-library',390,900,'phone','library','dark'],
 ['phone-more',390,900,'phone','more','dark'],
 ['phone-320',320,1000,'phone','home','dark'],
 ['tablet-home',1024,1000,'tablet','home','dark'],
 ['tv-collapsed',1920,1080,'tv','home','dark'],
 ['tv-expanded',1920,1080,'tv','home','dark',true],
 ['terminal-layout-dark',1440,1080,'desktop','home','dark',false,'terminal'],
 ['classic-layout-light',1440,1080,'desktop','home','light',false,'classic']
];
const manifest={kind:'design-prototype',nativeQualification:false,sourceCommit:source,rendererSha256:rendererHash,referenceDate:'2026-10-17',reproduction:'PLAYWRIGHT_MODULE=<installed-playwright> SHARP_MODULE=<installed-sharp> node scripts/meridian/preview.cjs',derivatives:[],references:[{platform:'web',url:'https://support.plex.tv/articles/200484203-interface-overview/',inspected:'2026-10-17',access:'search-index text; direct fetch 403'},{platform:'phone',url:'https://support.plex.tv/articles/navigating-the-mobile-apps/',inspected:'2026-10-17',access:'search-index text; direct fetch 403'},{platform:'TV',url:'https://forums.plex.tv/t/navigation-updates-coming-to-preview-users/941751',published:'2026-08-17',inspected:'2026-10-17',access:'direct text and published image visually inspected'}],fixture:'original synthetic titles and vector/CSS art; Cinema icon and existing Catalog glyph set',captures:[],checks:[]};
const safe=s=>s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/"/g,'&quot;');
(async()=>{
 fs.mkdirSync(out,{recursive:true});fs.copyFileSync(path.join(root,'crates/plurxd/src/web/icons/icon-192.png'),path.join(out,'cinema-icon.png'));
 const browser=await chromium.launch({headless:true});
 try{
  for(const [id,width,height,platform,route,mode,panel=false,theme='meridian'] of specs){
   const active=themes[theme][mode]||themes[theme].dark;
   const vars={...active,'--selected-bg':active['--selected-bg']||active['--text'],'--selected-ink':active['--selected-ink']||active['--bg'],'--action-bg':active['--accent'],'--action-ink':active['--btn-ink'],'--hero-scrim':active['--bg'],'--hero-ink':active['--text'],'--focus-ring':active['--accent'],'--focus-gap':active['--bg']};
   const cfg={label:platform==='tv'?'TV adaptation (tvOS / Android TV)':platform==='phone'?'Phone adaptation (iPhone / Android)':platform==='tablet'?'Tablet adaptation (iPad / Android)':'Desktop web',platform,route,mode,panel,themeName:themes[theme].name,icons};
   let html=template.replace('<style>','<style>:root{'+Object.entries(vars).map(([k,v])=>`${k}:${v}`).join(';')+'}');
   html=html.replace('<script>','<script id="config" type="application/json">'+JSON.stringify(cfg).replace(/</g,'\\u003c')+'</script><script>');
   fs.writeFileSync(path.join(out,id+'.html'),html);
   const page=await browser.newPage({viewport:{width,height},deviceScaleFactor:1,reducedMotion:'reduce'});
   const errors=[];page.on('pageerror',e=>errors.push(e.message));
   await page.goto('file://'+path.join(out,id+'.html'));
   await page.screenshot({path:path.join(out,id+'.png')});
   const geometry=await page.evaluate(()=>{
    const hero=document.querySelector('.hero'),copy=document.querySelector('.hero-copy'),buttons=copy?.querySelector('.actions');
    const visible=el=>{const r=el.getBoundingClientRect();return r.width>0&&r.height>0;};
    return {overflow:document.documentElement.scrollWidth>innerWidth,heroClipped:hero&&buttons?buttons.getBoundingClientRect().bottom>hero.getBoundingClientRect().bottom:false,bottomHorizontal:innerWidth>=600||document.querySelector('#bottom a:first-child').getBoundingClientRect().y===document.querySelector('#bottom a:last-child').getBoundingClientRect().y,visibleNavigation:Array.from(document.querySelectorAll('nav a')).filter(visible).map(el=>el.getAttribute('aria-label')),roles:Object.fromEntries(['--selected-bg','--selected-ink','--focus-ring','--action-bg','--action-ink','--hero-scrim','--hero-ink'].map(k=>[k,getComputedStyle(document.documentElement).getPropertyValue(k).trim()]))};
   });
   assert.deepEqual(errors,[],`${id} script errors`);assert.equal(geometry.overflow,false,`${id} horizontal overflow`);assert.equal(geometry.heroClipped,false,`${id} clipped hero actions`);assert.equal(geometry.bottomHorizontal,true,`${id} primary bar must be horizontal`);
   if(theme==='meridian' && route==='home' && !panel){
    const textBoxes=await page.evaluate(()=>Array.from(document.querySelectorAll('.hero-copy .eyebrow,.hero-copy h2,.hero-copy .meta,.hero-copy .synopsis')).map(el=>{const r=el.getBoundingClientRect();return {name:el.className||el.tagName,x:r.x,y:r.y,width:r.width,height:r.height,color:getComputedStyle(el).color.match(/[0-9.]+/g).slice(0,3).map(Number)};}));
    for(const extreme of [0,255]){
     await page.evaluate(extreme=>{document.body.classList.add('art-extreme');document.querySelector('.hero-art').style.background=`rgb(${extreme},${extreme},${extreme})`;document.querySelector('.hero-copy').style.visibility='hidden';},extreme);
     const {data,info}=await sharp(await page.screenshot()).removeAlpha().raw().toBuffer({resolveWithObject:true});
     for(const box of textBoxes){
      let minimum=Infinity;
      for(let y=Math.ceil(box.y);y<Math.min(Math.floor(box.y+box.height),height);y+=3)for(let x=Math.ceil(box.x);x<Math.min(Math.floor(box.x+box.width),width);x+=3){const at=(y*info.width+x)*info.channels;minimum=Math.min(minimum,contrast(box.color,[data[at],data[at+1],data[at+2]]));}
      check(`${id} rendered ${box.name} over ${extreme}`,minimum,4.5);
     }
     await page.evaluate(()=>document.querySelector('.hero-copy').style.visibility='');
     if(id==='desktop-home-dark'||id==='desktop-home-light'){const file=`${id}-art-${extreme}.png`;await page.screenshot({path:path.join(out,file)});manifest.derivatives.push({file,from:id,viewport:{width,height},appearance:mode,route,artworkExtreme:extreme,sourceCommit:source,rendererSha256:rendererHash,reproduction:manifest.reproduction});}
    }
   }
   manifest.captures.push({id,platform,runtime:`Chromium ${browser.version()} (HTML prototype)`,viewport:{width,height},layout:'Meridian design prototype',theme,appearance:mode,route,fixture:true,html:id+'.html',screenshot:id+'.png',sourceCommit:source,rendererSha256:rendererHash,geometry,reproduction:manifest.reproduction});
   await page.close();
  }
  // Render the two-tone focus treatment and worst-case hero samples separately.
  const swatches=[];
  for(const mode of ['dark','light']){
   const t=candidate.meridian[mode];
   swatches.push(`<section style="background:${t['--panel']};color:${t['--text']};padding:28px;border-radius:14px"><h2>${mode} — semantic states</h2><div style="display:flex;gap:28px;align-items:center"><button style="background:${t['--selected-bg']};color:${t['--selected-ink']};box-shadow:0 0 0 3px ${t['--bg']};outline:3px solid ${t['--accent']};outline-offset:3px">Selected + focused</button><button style="background:${t['--accent']};color:${t['--btn-ink']}">Primary action</button><button disabled style="background:${t['--panel2']};color:${t['--muted']};opacity:1">Disabled · unavailable</button></div>${[0,255].map(extreme=>`<div style="margin-top:24px;padding:20px;background:rgb(${[0,1,2].map(i=>Math.round(rgb(t['--bg'])[i]*.94+extreme*.06)).join(',')});color:${t['--text']}"><b>Hero over ${extreme===0?'black':'white'} artwork · 94% scrim floor</b><p style="color:${t['--muted']}">Body and muted text stay legible at the artwork extremes.</p></div>`).join('')}</section>`);
  }
  fs.writeFileSync(path.join(out,'semantic-states.html'),`<!doctype html><meta charset="utf-8"><title>M0 semantic state samples</title><style>body{background:#dadada;margin:0;padding:24px;display:grid;gap:24px;font:16px system-ui}button{border:0;padding:14px 20px;border-radius:24px;font:inherit}h1{font-size:18px;margin:0}h2{font-size:20px;margin:0 0 28px}</style><h1>M0 design prototype · synthetic contrast/focus samples · not native acceptance</h1>${swatches.join('')}`);
  const samplePage=await browser.newPage({viewport:{width:1200,height:850}});await samplePage.goto('file://'+path.join(out,'semantic-states.html'));await samplePage.screenshot({path:path.join(out,'semantic-states.png'),fullPage:true});await samplePage.close();
  // Deterministic gallery, with each frame retaining its visible prototype label.
  const gallery=`<!doctype html><meta charset="utf-8"><title>Meridian M0 gallery</title><style>body{margin:0;padding:40px;background:#eceae5;color:#242429;font:16px system-ui}h1{font-size:36px;letter-spacing:-1px;margin:0}p{max-width:980px;color:#56565e}h2{font-size:20px;margin:0 0 12px}.grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:28px}article{background:white;border-radius:14px;padding:20px}img{display:block;width:100%;height:auto;border-radius:7px}a{color:inherit}small{display:block;margin:12px 0;color:#56565e}.phone img{max-width:270px;margin:auto}.tv{grid-column:span 2}.pair{display:grid;grid-template-columns:1fr 1fr;gap:24px}</style><h1>Meridian / Cinema</h1><p>M0 design checkpoint · 17 October 2026 · original synthetic titles and art. These browser-rendered prototypes describe the intended web and native adaptations. They do not qualify iOS, tvOS or Android. Base ${source.slice(0,12)}.</p><div class="grid">${['desktop-home-dark','desktop-home-light','phone-home-dark','phone-home-light','tablet-home','desktop-library','tv-collapsed','tv-expanded','desktop-detail','phone-more','terminal-layout-dark','classic-layout-light','semantic-states'].map(id=>`<article class="${id.startsWith('tv')?'tv':id.startsWith('phone')?'phone':''}"><h2>${safe(id.replace(/-/g,' '))}</h2><a href="${id}.html"><img src="${id}.png" alt="${safe(id)} design prototype"></a><small>Design prototype · synthetic fixture · <a href="${id}.html">Open full render</a></small></article>`).join('')}</div>`;
  fs.writeFileSync(path.join(out,'gallery.html'),gallery);
  const galleryPage=await browser.newPage({viewport:{width:1600,height:1000}});await galleryPage.goto('file://'+path.join(out,'gallery.html'));await galleryPage.screenshot({path:path.join(out,'gallery.png'),fullPage:true});await galleryPage.close();
  const sheet=`<!doctype html><meta charset="utf-8"><style>body{margin:0;padding:32px;background:#eceae5;color:#242429;font:16px system-ui}h1{font-size:32px;margin:0 0 6px}p{margin:0 0 24px}.grid{display:grid;grid-template-columns:1fr 1fr;gap:24px}img{width:100%;display:block}article{padding:18px;background:white;border-radius:14px}h2{font-size:18px;margin:0 0 12px}.phone{grid-row:span 2;display:flex;flex-direction:column;align-items:center}.phone img{width:300px}.tv{grid-column:span 2}.tv img{width:100%}</style><h1>Meridian / Cinema</h1><p>M0 · design prototypes · synthetic fixtures · native acceptance remains outstanding</p><div class="grid"><article><h2>Desktop · collapsible sources + top search</h2><img src="desktop-home-dark.png"></article><article class="phone"><h2>Phone · persistent primary destinations</h2><img src="phone-home-dark.png"></article><article><h2>Light appearance · same semantic roles</h2><img src="desktop-home-light.png"></article><article class="tv"><h2>TV · expanded navigation, selected Home, focused Movies</h2><img src="tv-expanded.png"></article></div>`;
  fs.writeFileSync(path.join(out,'contact-sheet.html'),sheet);
  const sheetPage=await browser.newPage({viewport:{width:1500,height:1000}});await sheetPage.goto('file://'+path.join(out,'contact-sheet.html'));await sheetPage.screenshot({path:path.join(out,'contact-sheet.png'),fullPage:true});await sheetPage.close();
  for(const [file,width,height] of [['semantic-states.png',1200,850],['gallery.png',1600,null],['contact-sheet.png',1500,null]])manifest.derivatives.push({file,viewport:{width,height},kind:'labeled design composite',sourceCommit:source,rendererSha256:rendererHash,reproduction:manifest.reproduction});
  manifest.checks=audit;fs.writeFileSync(path.join(out,'manifest.json'),JSON.stringify(manifest,null,2)+'\n');
  console.log(JSON.stringify({sourceCommit:source,captures:manifest.captures.length,semanticChecks:audit.length,minRatio:Math.min(...audit.filter(x=>x.threshold===4.5).map(x=>x.ratio)),gallery:path.join(out,'gallery.html'),contactSheet:path.join(out,'contact-sheet.png')},null,2));
 }finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
