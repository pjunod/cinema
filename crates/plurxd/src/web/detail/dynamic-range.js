"use strict";
// ---- dynamic range: source, delivered format and display evidence ----------
// MEDIA-BADGES-PLAN.md §2. The chip above answers "what is this file?"; during
// playback the viewer is asking "what am I getting?", and those two answers
// differ constantly — a DV disc remux tone-mapped to an SDR H.264 transcode
// still showed a full-colour "DV P7".
//
// Asked live for the badge's capability note; negotiation keeps its existing
// boot-time answer. CSS dynamic-range describes capability, not whether HDR
// mode is active or the actual HDMI/display output. A missing query means the
// browser did not report HDR capability, not an observed SDR rendering path.
function displayIsHdr(){
  try{ return !!(window.matchMedia && window.matchMedia("(dynamic-range: high)").matches); }
  catch(e){ return false; }
}
// The coarse grade of a source, in the server's own vocabulary, so source and
// `delivered_dynamic_range` compare by string equality. hdrChip accepts a file
// whose `hdr` was never set but whose `hdr_format` names a grade, so this has
// to read both.
function sourceDynamicRange(f){
  const s=f.hdr_format||"";
  if(f.hdr==='dolby_vision'||/dolby/i.test(s)) return 'dolby_vision';
  if(f.hdr) return f.hdr;
  if(/hlg/i.test(s)) return 'hlg';
  return s?'hdr10':null;                                 // "HDR10+" / "HDR10"
}
// The source's Dolby Vision profile as a number.
//
// Column first, label second — the same order `playback::dolby_vision_profile`
// asks on the server, and it has to be the same order: the delivered profile
// beside it is derived from the column, so reading the label here would let a
// row whose two disagree invent an arrow ("DV P7 → DV P8") over a stream
// nothing converted. The label stays as the fallback for rows scanned before
// the columns existed.
//
// Null means no profile is known — a pre-column row, or a file that is not
// Dolby Vision — in which case there is nothing to compare against and the
// chip stays as it was rather than guessing.
function sourceDolbyVisionProfile(f){
  const column=f&&f.dolby_vision&&f.dolby_vision.profile;
  if(column) return Number(column);
  const m=/profile\s*(\d+)/i.exec((f&&f.hdr_format)||"");
  return m?Number(m[1]):null;
}
const RANGE_SHORT={dolby_vision:'DV',hdr10:'HDR10',hlg:'HLG',sdr:'SDR'};
const RANGE_LONG={dolby_vision:'Dolby Vision',hdr10:'HDR10',hlg:'HLG',sdr:'SDR'};
// The server's reason string already says *why* ("Dolby Vision metadata
// removed for this device; compatible HDR base kept") — better than anything
// this file could invent. Only borrow one that is actually about the picture.
function dynamicRangeReason(){
  try{
    if(typeof PLAYER==='undefined'||!PLAYER||!PLAYER.reasons) return null;
    return PLAYER.reasons.find(r=>r&&/dolby vision|hdr/i.test(r))||null;
  }catch(e){ return null; }
}
// (Source file, server-reported delivery, live browser capability) → badge.
// Arrows describe the delivered grade/profile; CSS never supplies a rendered
// grade. Ordinary browsers expose no observation of the actual display output.
// Missing delivery keeps the source-only chip rather than inventing a stream.
function dynamicRangeBadge(f, delivered, displayHdr, deliveredDvProfile){
  f=f||{};
  const chip=hdrChip(f); if(!chip) return null;
  const source=sourceDynamicRange(f);
  const lit={cls:chip.cls, text:chip.text, base:chip.text, arrow:null, full:chip.full,
    aria:chip.full||chip.text, panel:RANGE_LONG[source]||chip.text,
    off:false, source, rendered:null};
  if(!delivered) return lit;
  const capability=displayHdr?"browser reports HDR capability":"browser has not reported HDR capability";
  const output="display output unverified";
  const grade=RANGE_LONG[delivered]||String(delivered).toUpperCase();
  const profile=delivered==='dolby_vision'&&deliveredDvProfile
    ?` Profile ${deliveredDvProfile}`:"";
  const delivery=`Delivered ${grade}${profile}`;
  const evidence=`${delivery} · ${output} · ${capability}`;
  if(delivered===source){
    const sourceDv=sourceDolbyVisionProfile(f);
    if(source==='dolby_vision'&&deliveredDvProfile&&sourceDv&&deliveredDvProfile!==sourceDv){
      const arrow=`DV P${deliveredDvProfile}`;
      return {...lit,text:`${chip.text} → ${arrow}`,arrow,
        full:`${chip.full} — ${evidence} (converted for this browser)`,
        aria:`Dolby Vision Profile ${sourceDv}; ${evidence}`,
        panel:`${evidence} — converted for this browser`};
    }
    return {...lit,full:`${chip.full||chip.text} — ${evidence}`,
      aria:`${chip.full||chip.text}; ${evidence}`,panel:evidence};
  }
  // A server delivery change can explain a conversion; a CSS answer cannot.
  const note=dynamicRangeReason()||(delivered==='sdr'
    ?`tone-mapped from ${RANGE_LONG[source]||'the source grade'}`
    :source==='dolby_vision'?"Dolby Vision removed for this browser":null);
  const arrow=RANGE_SHORT[delivered]||String(delivered).toUpperCase();
  return {...lit,text:`${chip.text} → ${arrow}`,arrow,
    full:`${chip.full||chip.text} — ${evidence}${note?` (${note})`:""}`,
    aria:`${RANGE_LONG[source]||chip.text}; ${evidence}`,
    panel:`${evidence}${note?` — ${note}`:""}`,off:true};
}
// The badge for whatever this player is doing right now.
//
// One reader of `PLAYER`, so the four surfaces that paint the chip — the fact
// badges, the play overlay, the stats panel and the debug ledger — cannot pass
// `dynamicRangeBadge` three of its four arguments between them. Dropping one
// silently degrades the chip to the state it had before that argument existed,
// which is a correct-looking badge for the wrong delivery.
function effectiveHdrProcessing(report,generation,delivered){
  const sdr=delivered==='sdr'&&report?.sdr_processed===true&&report?.hdr10_enhanced===false;
  if(!report||(!sdr&&(report.hdr10_enhanced!==true||report.sdr_processed===true||delivered!=='hdr10'))||typeof report.fel_contributed!=='boolean'
    ||!['hdr10','sdr'].includes(delivered)||typeof generation!=='string'||report.generation!==generation
    ||!/^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i.test(generation)
    ||!Array.isArray(report.applied_operations)||!report.applied_operations.length
    ||report.applied_operations.length>16
    ||!report.applied_operations.every(op=>typeof op==='string'?op.length>0&&op.length<=64
      :op&&typeof op==='object'&&Object.keys(op).length===1
        &&op.DisplayTargetMapping&&Object.keys(op.DisplayTargetMapping).length===1
        &&(sdr?[100]:[400,600,1000,1600,2000,4000]).includes(op.DisplayTargetMapping.nits))
    ||sdr&&(!report.applied_operations.includes('Bt709Gamma24Conversion')||!report.applied_operations.some(op=>op?.DisplayTargetMapping?.nits===100))) return null;
  return report;
}
function effectiveProcessingDetail(report){
  const names={PolynomialReshape:'RPU polynomial mapping',MmrReshape:'RPU MMR mapping',
    RpuColorConversion:'RPU color conversion',TargetMapping:'target mapping',CreativeTrimApplication:'creative trims'};
  const metadata=report.applied_operations.map(op=>names[op]).filter(Boolean).join(', ');
  return `${report.sdr_processed?`Dolby Vision–processed SDR (BT.709 gamma 2.4, ${report.applied_operations.includes('CreativeTrimApplication')?'creative trims applied':report.creative_trims_partial===true?'creative trims on some shots':'L1 only'})`:'Dolby Vision–enhanced HDR10'} · FEL used: ${report.fel_contributed?'yes':'no'}${metadata?` · Applied: ${metadata}`:''}`;
}
function clearEffectiveProcessing(p){
  if(!p) return;
  const changed=!!p.effectiveProcessing;
  p.effectiveProcessing=null;
  if(changed&&typeof PLAYER!=='undefined'&&p===PLAYER&&typeof renderPlayerInfo==='function') renderPlayerInfo();
}
// The Dolby Vision enhancement the server says this session's picture carries
// (StartResponse.enhancement, crates/plurx-core/src/playback/enhancement.rs).
// The server computes it so every client renders the same pills and lines
// rather than reconstructing the delivery route; this only refuses what it
// cannot render. Absent (no Dolby Vision, an old server) is null; an SDR
// play has details and no active badges.
function playbackEnhancementFromWire(raw){
  const known=['FEL','RPU','DYN'];
  if(!raw||typeof raw!=='object'||!Array.isArray(raw.active)||!Array.isArray(raw.details)) return null;
  const active=[...new Set(raw.active.filter(badge=>known.includes(badge)))];
  // A detail line for a badge this client has no pill for (a newer server's
  // enhancement) still says something true, so the Playback info row keeps its
  // text; only the pills are limited to the badges `known` lists.
  const details=raw.details.filter(d=>d&&typeof d.badge==='string'&&typeof d.text==='string'
    &&d.text.length>0&&d.text.length<=240).map(d=>({badge:d.badge,active:d.active===true,text:d.text}));
  return {active,details,requires_effective_processing:raw.requires_effective_processing===true};
}
// The current, generation-matching HDR10-E report — the same reading
// playerRangeBadge makes, for a caller that is not painting the range chip.
function playerEffectiveProcessing(p){
  if(!p) return null;
  const stream=typeof statsStreamFormat==="function"?statsStreamFormat(p):null;
  return effectiveHdrProcessing(p.effectiveProcessing,p.controlReporter?.bootstrap?.generation,
    stream?.dynamic_range||p.deliveredRange);
}
// What the player may show now. A processed delivery's FEL/RPU/DYN claim is
// only true while the report proving it is current: a seek, fallback or
// replacement clears the report, and with it every pill and the details row.
function playerEnhancement(p){
  const enhancement=p&&p.enhancement;
  if(!enhancement) return null;
  if(!enhancement.requires_effective_processing) return enhancement;
  const report=playerEffectiveProcessing(p);
  if(!report) return null;
  // Session creation can precede the first processed window. For these
  // routes the current execution report supersedes that provisional snapshot;
  // carried Dolby Vision routes keep the server-authored enhancement above.
  const operations=report.applied_operations;
  const rpuNames={PolynomialReshape:'polynomial reshaping',MmrReshape:'MMR reshaping',RpuColorConversion:'RPU color conversion'};
  const rpu=operations.filter(op=>typeof op==='string'&&Object.hasOwn(rpuNames,op)).map(op=>rpuNames[op]);
  const target=operations.find(op=>op?.DisplayTargetMapping)?.DisplayTargetMapping.nits;
  const trims=operations.includes('CreativeTrimApplication')?"with the colourist's creative trims"
    :report.creative_trims_partial===true?"with the colourist's creative trims on some shots":"L1 only; creative trims are not applied";
  const current=[
    {badge:'FEL',active:report.fel_contributed,text:report.fel_contributed
      ?`FEL: full enhancement layer reconstructed into the ${report.sdr_processed?'SDR ':''}picture`
      :'FEL: no enhancement-layer detail in this window'},
    {badge:'RPU',active:rpu.length>0,text:rpu.length?`RPU: ${rpu.join(', ')} applied by the server`:'RPU: no reshaping or colour conversion applied in this window'},
    {badge:'DYN',active:!!target,text:target?`DYN: per-shot Dolby display mapping for a ${target}-nit display by the server (${trims})`
      :'DYN: no per-shot Dolby display mapping applied in this window'}
  ];
  const details=current.filter(d=>d.badge!=='FEL'||d.active||enhancement.details.some(old=>old.badge==='FEL'))
    .concat(enhancement.details.filter(d=>!['FEL','RPU','DYN'].includes(d.badge)));
  return {...enhancement,active:current.filter(d=>d.active).map(d=>d.badge),details};
}
// The Playback info row: the active badges, then one neutral line per
// enhancement — active or not — from the server or current execution report.
function playerEnhancementRow(p){
  const enhancement=playerEnhancement(p);
  if(!enhancement) return null;
  return {value:enhancement.active.length?enhancement.active.join(' · '):'None active',
    note:enhancement.details.length?enhancement.details.map(d=>d.text).join('\n'):null};
}
function enhancementPills(enhancement){
  if(!enhancement||!enhancement.active.length) return '';
  return enhancement.active.map(badge=>{
    const detail=enhancement.details.find(d=>d.badge===badge&&d.active);
    const label=detail?detail.text:badge;
    return `<span class="vbadge enh" title="${esc(label)}" aria-label="${esc(label)}">${esc(badge)}</span>`;
  }).join('');
}
function playerRangeBadge(f){
  const stream=typeof statsStreamFormat==="function"?statsStreamFormat(PLAYER):null;
  const delivered=stream?.dynamic_range||PLAYER.deliveredRange;
  const badge=dynamicRangeBadge(f, delivered, displayIsHdr(), PLAYER.deliveredDvProfile);
  const generation=PLAYER.controlReporter?.bootstrap?.generation;
  const report=effectiveHdrProcessing(PLAYER.effectiveProcessing,generation,delivered);
  if(!report) return badge;
  const mark=report.sdr_processed?'SDR-E':'HDR10-E';
  const source=sourceDynamicRange(f),same=source===(report.sdr_processed?'sdr':'hdr10');
  const detail=effectiveProcessingDetail(report);
  const base=same?mark:badge?.base||mark;
  return {...(badge||{cls:'hdr',source,rendered:null,off:false}),base,
    text:same||!badge?mark:`${base} → ${mark}`,arrow:same||!badge?null:mark,
    full:`${detail} · display output unverified`,aria:detail,panel:detail};
}
function premiumAudio(f){ const cs=(f.audio_streams||[]).map(a=>(a.codec||'').toLowerCase());
  if(cs.some(c=>c.includes('truehd'))) return 'TrueHD';
  if(cs.some(c=>c.includes('dts'))) return 'DTS';
  if(cs.some(c=>c.includes('eac3'))) return 'DD+';
  return null; }
function specBadges(f){
  const b=[];
  const r=resLabel(f.width,f.height); if(r) b.push(`<span class="vbadge res">${mediaIcon('resolution')}${r}</span>`);
  const c=codecLabel(f.video_codec); if(c) b.push(`<span class="vbadge codec">${mediaIcon('video')}${esc(c)}</span>`);
  if(f.bit_depth>=10) b.push(`<span class="vbadge depth">${f.bit_depth}-bit</span>`);
  const h=hdrChip(f); if(h) b.push(`<span class="vbadge ${h.cls}" title="${esc(h.full||h.text)}">${mediaIcon('dynamic')}${esc(h.text)}</span>`);
  // Keep the established movie badge policy (only premium audio) while giving
  // an audio-only source a useful primary-codec badge.
  const au=premiumAudio(f)||(!f.video_codec&&(f.audio_streams||[])[0]&&codecLabel(f.audio_streams[0].codec));
  if(au) b.push(`<span class="vbadge audio">${mediaIcon('audio')}${esc(au)}</span>`);
  return b.join("");
}
function playbackAudioChip(a){
  if(!a) return null;
  const codec=(a.codec||'').toLowerCase(), title=a.title||'';
  let mark='', full='';
  if(/atmos/i.test(title)){ mark='ATMOS'; full='Dolby Atmos'; }
  else if(codec==='eac3'||codec==='e-ac-3'){ mark='DD+'; full='Dolby Digital Plus'; }
  else if(codec==='ac3'||codec==='ac-3'){ mark='DD'; full='Dolby Digital'; }
  else if(codec==='truehd'){ mark='TRUEHD'; full='Dolby TrueHD'; }
  else if(codec==='dts'||codec==='dca'){ mark='DTS'; full='DTS'; }
  else if(codec){ mark=codec.toUpperCase(); full=mark; }
  if(!mark) return null;
  const channels=fmtChannels(a.channels), text=[mark,channels].filter(Boolean).join(' ');
  return {text,full:[full,channels].filter(Boolean).join(' ')};
}
function playerFactBadges(){
  if(!PLAYER) return '';
  const s=PLAYER.source||{}, b=[];
  const r=resLabel(s.width,s.height); if(r) b.push(`<span class="vbadge res">${mediaIcon('resolution')}${r}</span>`);
  // Source vs delivered vs rendered, not source alone — this row is on screen
  // during playback, where "what am I getting?" is the question. The detail
  // page's specBadges() deliberately stays source-only: there is no session to
  // report on there.
  const h=playerRangeBadge(s);
  if(h) b.push(`<span class="vbadge ${h.cls}${h.off?' off':''}" title="${esc(h.full||h.text)}" aria-label="${
    esc(h.aria||h.text)}">${mediaIcon('dynamic')}<span class="vsrc">${esc(h.base)}</span>${
    h.arrow?`<span class="varrow">→ ${esc(h.arrow)}</span>`:''}</span>`);
  // Active Dolby Vision enhancements only, right after the range chip they
  // qualify; the reasons for the inactive ones live in the details panel.
  const enhancement=enhancementPills(playerEnhancement(PLAYER));
  if(enhancement) b.push(enhancement);
  const au=playbackAudioChip(PLAYER.audio&&PLAYER.audio[PLAYER.curAudio]);
  if(au) b.push(`<span class="vbadge audio" title="${esc(au.full)}">${mediaIcon('audio')}${esc(au.text)}</span>`);
  return b.join('');
}
