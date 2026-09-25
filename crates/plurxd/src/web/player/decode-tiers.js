"use strict";
// ---- runtime playback capabilities ----------------------------------------
// What THIS browser can actually decode, probed once. Sent on every decision so
// the server direct-plays/remuxes whatever the browser supports and only
// transcodes what it genuinely can't — Safari keeps HEVC, everyone keeps their
// own audio codecs, etc. HDR is claimed only on an HDR display (else the server
// tone-maps, since HDR-on-SDR looks washed-out).

// ---- HEVC decode tiers -----------------------------------------------------
// The one question this file used to ask about HEVC was `hvc1.1.6.L93.B0` —
// Main, 8-bit, level 3.1 (720p-class) — and a yes was reported to the server as
// blanket `hevc` support with no height at all. That is probe-low/claim-high,
// the exact shape that already cost this project a black picture with working
// sound once (see MSE_VIDEO's comment further down for the transport half of
// the same story): a 4K 10-bit Main10 stream handed to a decoder that only does
// 8-bit at 720p is accepted by every layer above the decoder and rendered as
// nothing. No error fires, so nothing falls back.
//
// So ask the two questions the server actually needs answered — how many
// pixels, and how many bits — instead of one question that answers neither.
// The profile field in the codec string is the ONLY place a decoder's answer
// distinguishes bit depth (`hvc1.1.6.*` is Main/8-bit, `hvc1.2.4.*` is Main10),
// and the level field is the height axis: L93 = 3.1 (720p), L120 = 4.0 (1080p),
// L153 = 5.1 (4K). The 8-bit rungs above L93 are here so an ordinary 8-bit-only
// decoder still reports the 1080p/4K ceiling it really has rather than being
// pinned at 720p by the lowest rung we happen to ask about first.
//
// width/height/bitrate/framerate are only read by the MediaCapabilities path
// (they are required members of its VideoConfiguration); the isTypeSupported
// fallback has nowhere to put them.
const HEVC_TIERS=[
  {codec:"hvc1.1.6.L93.B0",  depth:8,  width:1280, height:720,  bitrate:4000000},
  {codec:"hvc1.1.6.L120.B0", depth:8,  width:1920, height:1080, bitrate:8000000},
  {codec:"hvc1.1.6.L153.B0", depth:8,  width:3840, height:2160, bitrate:25000000},
  {codec:"hvc1.2.4.L120.B0", depth:10, width:1920, height:1080, bitrate:10000000},
  {codec:"hvc1.2.4.L153.B0", depth:10, width:3840, height:2160, bitrate:30000000},
];
// Fold a per-tier pass/fail table into the three facts that go on the wire.
//
// `maxheight` is deliberately the height at which EVERY claimed HEVC profile
// decodes, not the height of the tallest rung that passed. The server's
// `max_height` is one codec-agnostic number (plurx-core/src/playback: a file
// taller than it is transcoded, whatever its codec), so a client that decodes
// 8-bit at 4K but Main10 only at 1080p cannot say "2160" and "hevc10" in the
// same breath — that pair is precisely the black-screen claim. Taking the min
// costs such a device a needless transcode of 4K 8-bit HEVC; over-claiming
// costs it the picture. `pqPassed` is null when the probe had no way to ask
// about the transfer function (isTypeSupported has no such axis).
function hevcTierSummary(passed, pqPassed){
  let depth8=0, depth10=0, pq=0;
  HEVC_TIERS.forEach((t,i)=>{
    if(!passed||!passed[i]) return;
    if(t.depth>=10){
      if(t.height>depth10) depth10=t.height;
      if(pqPassed&&pqPassed[i]&&t.height>pq) pq=t.height;
    }else if(t.height>depth8) depth8=t.height;
  });
  const maxheight=(depth8&&depth10)?Math.min(depth8,depth10):(depth8||depth10);
  return {depth8, depth10, maxheight, pq10:!!(maxheight&&pq>=maxheight)};
}
// The fallback probe: `canPlayType`/`isTypeSupported`, one call per rung. Both
// are synchronous and neither knows anything about transfer functions, so this
// path can establish depth and height and never HDR presentation.
function hevcTiersSync(can, mse){
  return HEVC_TIERS.map(t=>{
    const type=`video/mp4; codecs="${t.codec}"`;
    return can(type)||mse(type);
  });
}

// Progressive packaging is a separate claim from decoder availability. A
// MediaSource success cannot authorize a plain <video src>, and a successful
// hvc1 probe says nothing about hev1 (or either Dolby Vision label).
async function progressiveHevcSampleEntries(claimedTiers, claimedPqTiers, dvProfiles, injected){
  const probe=injected||{};
  let mc=probe.mediaCapabilities;
  if(mc===undefined){ try{ mc=navigator&&navigator.mediaCapabilities; }catch(e){ mc=null; } }
  let video=probe.videoElement;
  if(video===undefined){ try{ video=document.createElement("video"); }catch(e){ video=null; } }
  const can=type=>{ try{ return !!video&&video.canPlayType(type)!==""; }catch(e){ return false; } };
  const fileAnswer=async(type,width,height,bitrate,transferFunction)=>{
    if(!mc||typeof mc.decodingInfo!=="function") return null;
    const config={contentType:type,width,height,bitrate,framerate:24};
    if(transferFunction) config.transferFunction=transferFunction;
    try{
      const answer=await mc.decodingInfo({type:"file",video:config});
      return answer&&typeof answer.supported==="boolean"?answer.supported:null;
    }catch(e){ return null; }
  };
  const admitted=[];
  const summary=hevcTierSummary(claimedTiers,claimedPqTiers);
  const depths=summary.depth10?[8,10]:(summary.depth8?[8]:[]);
  const required=[];
  for(const depth of depths){
    let profile=HEVC_TIERS.map((tier,index)=>({tier,index}))
      .filter(row=>row.tier.depth===depth&&row.tier.height<=summary.maxheight);
    // Main10's lowest configured probe is 1080p. If the shared ceiling was
    // lowered below that by Main, still require the lowest Main10 file probe:
    // the wire document emits both profiles whenever Main10 is claimed.
    if(!profile.length){
      const first=HEVC_TIERS.findIndex(tier=>tier.depth===depth);
      if(first>=0) profile=[{tier:HEVC_TIERS[first],index:first}];
    }
    required.push(...profile.map(row=>row.index));
  }
  for(const tag of ["hvc1","hev1"]){
    let complete=true;
    for(const i of required){
      const tier=HEVC_TIERS[i];
      const codec=tier.codec.replace(/^hvc1/,tag);
      const type=`video/mp4; codecs="${codec}"`;
      const answer=await fileAnswer(type,tier.width,tier.height,tier.bitrate,null);
      if(answer===false || (answer===null&&!can(type))){ complete=false; break; }
      // `canPlayType` has no transfer-function axis. If the general ladder
      // claimed PQ for this rung, only a progressive file answer for PQ can
      // extend the sample-entry claim to that presentation.
      if(summary.pq10&&tier.depth>=10){
        const pq=await fileAnswer(type,tier.width,tier.height,tier.bitrate,"pq");
        if(pq!==true){ complete=false; break; }
      }
    }
    if(required.length&&complete) admitted.push(tag);
  }
  const dvCodec=(tag,profile)=>`${tag}.${profile===5?"05.06":"08.07"}`;
  const profiles=(dvProfiles||[]).filter(profile=>profile===5||profile===8);
  for(const tag of ["dvh1","dvhe"]){
    if(!profiles.length) continue;
    let complete=true;
    for(const profile of profiles){
      const type=`video/mp4; codecs="${dvCodec(tag,profile)}"`;
      const answer=await fileAnswer(type,3840,2160,30000000,"pq");
      if(answer===false || (answer===null&&!can(type))){ complete=false; break; }
    }
    if(complete) admitted.push(tag);
  }
  return admitted;
}
// The real probe. `navigator.mediaCapabilities.decodingInfo` is the only API
// that takes a resolution, a bitrate and a transfer function and answers about
// the combination — which is what a decoder ceiling actually is. Each Main10
// rung is asked twice: once plain (does the decoder do 10-bit at this size?)
// and once with `transferFunction:'pq'` (can this machine PRESENT HDR10 at that
// size?). Those differ: a 10-bit SDR anime rip decodes on hardware that cannot
// put PQ on the wire, and folding the two together would either lose that file
// to a transcode or claim HDR the display never gets.
//
// Returns null when the API is missing or every single query threw, which is
// the caller's signal to use the synchronous ladder instead. It is async, and
// that is why CAPS_Q is rebuilt rather than computed once: see PLAY_CAPS_READY.
async function hevcTiersMediaCapabilities(){
  let mc=null;
  try{ mc=navigator&&navigator.mediaCapabilities; }catch(e){}
  if(!mc||typeof mc.decodingInfo!=="function") return null;
  let answered=false;
  // `media-source` first because segmented remuxes are the demanding path, then
  // `file` for progressive direct play — the same OR the sync probe does across
  // isTypeSupported/canPlayType, so the two paths cannot disagree by shape.
  const ask=async(t,pq)=>{
    const video={contentType:`video/mp4; codecs="${t.codec}"`, width:t.width,
      height:t.height, bitrate:t.bitrate, framerate:24};
    if(pq) video.transferFunction="pq";
    for(const type of ["media-source","file"]){
      try{
        const r=await mc.decodingInfo({type, video});
        answered=true;
        if(r&&r.supported) return true;
      }catch(e){}
    }
    return false;
  };
  const passed=[], pqPassed=[];
  for(const t of HEVC_TIERS){
    const ok=await ask(t,false);
    passed.push(ok);
    pqPassed.push(!!(ok&&t.depth>=10&&await ask(t,true)));
  }
  return answered?{passed, pqPassed}:null;
}
function buildPlayCaps(hevc){
  let v=null; try{ v=document.createElement("video"); }catch(e){}
  const can=t=>{ try{ return !!v && v.canPlayType(t)!==""; }catch(e){ return false; } };
  const mse=t=>{ try{ return !!(window.MediaSource && MediaSource.isTypeSupported(t)); }catch(e){ return false; } };
  const vc=["h264"];
  // `hevc` still means "there is an HEVC decoder" and is what the server matches
  // against ffprobe's codec_name; `hevc10` is the bit-depth half of the same
  // answer, named to match the MSE_VIDEO ladder key so the two vocabularies stay
  // one vocabulary. Sending 8-bit-only as bare `hevc` is what stops a Main10
  // source from being direct-played to a decoder that would render it black.
  if(hevc.depth8||hevc.depth10) vc.push("hevc");
  if(hevc.depth10) vc.push("hevc10");
  if(can('video/mp4; codecs="av01.0.05M.08"')||mse('video/mp4; codecs="av01.0.05M.08"')) vc.push("av1");
  if(can('video/webm; codecs="vp9"')||mse('video/webm; codecs="vp9"')) vc.push("vp9");
  if(can('video/webm; codecs="vp8"')) vc.push("vp8");
  const ac=["aac","mp3"];
  if(can('audio/mp4; codecs="opus"')||mse('audio/webm; codecs="opus"')) ac.push("opus");
  if(can('audio/flac')||mse('audio/mp4; codecs="flac"')) ac.push("flac");
  if(can('audio/mp4; codecs="ac-3"')) ac.push("ac3");    // Safari: "maybe"; most others ""
  if(can('audio/mp4; codecs="ec-3"')) ac.push("eac3");
  // Asked once here because the SERVER only ever hears the boot-time answer —
  // it rides in CAPS_Q and there is no way to re-tell it mid-session. The badge
  // re-asks per render via the same displayIsHdr(); see its comment.
  const hdrDisplay=displayIsHdr();
  const hdr=hdrDisplay && (vc.includes("hevc")||vc.includes("av1"));
  // Dolby Vision, asked separately from HDR because the two answers differ
  // and the difference is a whole class of failure. A DV disc remux usually
  // carries an HDR10-compatible base layer, so a browser that reports HDR
  // looks able to take it — and Chrome then refuses the track outright
  // (MEDIA_ERR_DECODE), because what reaches its decoder is flagged Dolby
  // Vision whatever the base layer looks like. Safari decodes those. Without
  // this probe the server could not tell the two apart, remuxed to both, and
  // the error-fallback quietly re-encoded a 4K disc remux down to the Auto
  // rung on Chrome. `dvh1`/`dvhe` are the DV sample entries; a browser that
  // claims either can take the stream as it stands.
  const dvCan=p=>[`dvh1.${p}`,`dvhe.${p}`]
    .some(c=>can(`video/mp4; codecs="${c}"`)||mse(`video/mp4; codecs="${c}"`));
  const dvProfiles=[];
  if(dvCan("05.06")) dvProfiles.push(5);
  if(dvCan("08.07")) dvProfiles.push(8);
  // `hdrDisplay` is exposed (it is not part of CAPS_Q) so the badge layer can
  // see what the server was told, separately from what is true right now.
  const containers=["mp4","webm","mov","m4a","m4b","mp3","aac"];
  if(can('audio/flac')) containers.push("flac");
  if(can('audio/ogg')) containers.push("ogg","opus");
  if(can('audio/wav')) containers.push("wav");
  return {vcodec:vc.join(","), acodec:ac.join(","), container:containers.join(","), hdr:hdr?1:0,
    dv:dvProfiles.length?1:0, dvprofile:dvProfiles.join(","), hdrDisplay,
    // Only ever the HEVC ceiling, and null when there is no HEVC decoder at
    // all: the server's max_height is codec-agnostic, so a number here caps
    // H.264 and AV1 too. Absent is today's behaviour and stays the answer for
    // every browser that was never going to be sent HEVC anyway.
    maxheight:hevc.maxheight||null,
    hdr10t:(hevc.pq10&&hdrDisplay)?1:0};
}
// The boot-time answer, from the synchronous ladder so that CAPS_Q exists
// before anything can ask for it. MediaCapabilities refines it below.
let PLAY_HEVC_TIERS=hevcTiersSync(
  t=>{ try{ const v=document.createElement("video"); return v.canPlayType(t)!==""; }catch(e){ return false; } },
  t=>{ try{ return !!(window.MediaSource && MediaSource.isTypeSupported(t)); }catch(e){ return false; } }
);
let PLAY_HEVC_PQ_TIERS=[];
let PLAY_CAPS=buildPlayCaps(hevcTierSummary(PLAY_HEVC_TIERS, null));
// Safe even before the async probe settles: [] is an explicit restrictive
// claim, never the legacy/unrestricted omission.
PLAY_CAPS.progressiveHevcSampleEntries=[];
// The capability query string every /decision and /stream.mp4 carries.
//
//   vcodec     CSV of decodable video codecs. `hevc` = an HEVC decoder exists;
//              `hevc10` = it does Main10 (10-bit). 8-bit-only clients send
//              `hevc` without `hevc10`.
//   acodec     CSV of decodable audio codecs.
//   container  CSV of containers playable from a plain <video src>.
//   hdr        1 when HDR may be sent as-is: an HDR-capable codec AND an HDR
//              display. 0 asks the server to tone-map.
//   dv         1 when this client decodes Dolby Vision at any profile.
//   dvprofile  CSV of the DV profiles actually probed, e.g. `5,8`.
//   maxheight  The tallest height every HEVC profile named in `vcodec` decodes.
//              Omitted entirely when no HEVC is claimed, because the server
//              applies max_height to every codec and an HEVC-derived cap must
//              not quietly become an H.264 cap on a client sent no HEVC.
//   hdr10t     1 means, exactly: this client can DECODE HEVC Main10 and PRESENT
//              it as PQ, at the height reported in `maxheight`, on the display
//              it is attached to right now. It is the conjunction of a
//              MediaCapabilities query carrying `transferFunction:'pq'` and
//              `(dynamic-range: high)`; either half failing sends 0, and so
//              does a browser with no MediaCapabilities at all, because the
//              fallback probe cannot ask about transfer functions and a guess
//              here is the over-claim this whole probe exists to stop. 0 is
//              therefore "not proven", never "proven false" — a server reading
//              it must treat 0 as the safe/tone-mapped path. It is strictly
//              narrower than `hdr`, which any HDR-capable codec satisfies.
function capsQuery(c){
  return `vcodec=${c.vcodec}&acodec=${c.acodec}&container=${c.container}&hdr=${c.hdr}`+
    `&dv=${c.dv}&dvprofile=${c.dvprofile}`+
    (c.maxheight?`&maxheight=${c.maxheight}`:"")+
    `&hdr10t=${c.hdr10t}`;
}
let CAPS_Q=capsQuery(PLAY_CAPS);

// The same claims as a document (docs/PLAYBACK.md, PLAYBACK-CAPS-V2-PLAN 4.1).
//
// It exists because the flat query above cannot say several true things at
// once. `maxheight` is one slot for a decoder whose 8-bit and 10-bit ceilings
// differ, so this browser sent the minimum of its two rungs and transcoded
// every 4K 8-bit title it would have direct-played. `hdr` and `hdr10t` are two
// bits describing one question — can this device PRESENT this curve — asked
// once globally and once for HEVC. And `dv=1` with no `dvprofile` claimed
// every Dolby Vision profile, including the dual-layer ones no consumer
// decoder takes.
//
// CAPS_Q is not retired: the progressive `play_url` still carries it, and the
// server keeps translating it. Both shapes reach one translation on the
// server, pinned by a test, so this browser cannot get two verdicts for one
// machine.
function capsDocument(c, limits){
  // One entry per (codec, profile-set) with its own ceiling. `hevc10` is this
  // app's name for the Main10 half of the HEVC answer; on the wire it is the
  // `main10` profile of `hevc`, because a profile is what it always was.
  const video=[];
  const codecs=String(c.vcodec||"").split(",").filter(Boolean);
  const hevcPresent=["sdr"]; if(c.hdr10t) hevcPresent.push("pq");
  for(const codec of codecs){
    if(codec==="hevc10") continue;              // folded into hevc, below
    const entry={codec, present:codec==="hevc"?hevcPresent:["sdr"]};
    if(codec==="hevc"){
      // Two entries when the ceilings differ, one when they do not. This is
      // the whole point of the document: the old wire had to pick a number.
      entry.profiles=["main"];
      if(c.maxheight) entry.max_height=c.maxheight;
      if(codecs.includes("hevc10")){
        video.push(entry);
        video.push({codec:"hevc", profiles:["main10"], present:hevcPresent,
                    ...(c.maxheight?{max_height:c.maxheight}:{})});
        continue;
      }
    }else if(c.maxheight && codec==="h264"){
      // `maxheight` was only ever the HEVC ceiling (see buildPlayCaps); it is
      // carried document-level below rather than pinned onto H.264 here.
    }
    video.push(entry);
  }
  const dvProfiles=String(c.dvprofile||"").split(",").filter(Boolean).map(Number).filter(n=>n>0);
  for(const entry of video){ if(entry.codec==="hevc") entry.dv_profiles=dvProfiles; }
  return {
    v:2,
    // The build this page was served by. The web app has no version of its
    // own — it ships inside the daemon — so the daemon's build IS this
    // client's build, and it is what the fleet counter on /system needs to
    // watch stragglers by. Bounded because it is reprinted into a log line.
    client:{kind:"web", build:String((SERVER&&SERVER.build)||"").slice(0,48),
            ua:String(navigator.userAgent||"").slice(0,160)},
    video,
    audio:String(c.acodec||"").split(",").filter(Boolean),
    containers:String(c.container||"").split(",").filter(Boolean),
    transports:["progressive","hls"],
    // No `subtitle_overlays`. This player has no PGS renderer, and absent is
    // never a claim — so the server offers it no PGS default, advertises no
    // `overlay` protocol, and tells it the truth when a viewer picks a bitmap
    // track: that delivery needs a burn-in. Add the key here the day a
    // renderer lands, not before.
    progressive_hevc_sample_entries:Array.isArray(c.progressiveHevcSampleEntries)
      ?c.progressiveHevcSampleEntries.slice(0,4):[],
    // Only when this browser has one; absent is not a claim.
    ...(c.dv?{dv_transport:"progressive"}:{}),
    display:{hdr:!!c.hdrDisplay, dolby_vision:dvProfiles.length>0},
    ...(c.maxheight?{max_height:c.maxheight}:{}),
    // The identity is the MAP KEY in localStorage, so the entries have to be
    // rebuilt with it inlined — the obvious `Object.values()` would send a
    // document whose every limit is anonymous, and an anonymous limit matches
    // nothing on the server. See `decodeLimits()`.
    // Bounded on both axes, because since M1 this document also rides the
    // create — which every seek and every audio switch sends. The server
    // keeps at most `MAX_LEARNED_LIMITS` (256) and clips each label to 160,
    // so anything past that is bytes on the wire that no reader will ever
    // see. Newest first, so a browser at the cap sends the limits that still
    // describe what it can play.
    learned_limits:Object.entries(limits||{})
      .sort((a,b)=>(Number(b[1].at)||0)-(Number(a[1].at)||0))
      .slice(0,256)
      .map(([identity, l])=>({
        identity, label:String(l.label||"").slice(0,160),
        lost:Number(l.lost)||0, secs:Number(l.secs)||0, rate:Number(l.rate)||0,
        at_ms:Number(l.at)||0,
      })),
  };
}
// The one document, for the two requests that send it.
//
// `/decision` and the create that acts on its answer have to be asked the
// same question. When they are not, the create has nothing to re-derive from
// and falls back to trusting the body's echo of the decision — and the echo
// cannot carry `convert_dolby_vision`, because no client has ever been able to
// ask for a conversion. The server derives that field for an echo-only create
// (`legacy_trusted_review`), but it is deriving it for a build that enumerated
// nothing; sending the document is what turns that guess back into an answer,
// and it is the only thing keeping `plan_derivation.legacy_trusted` off zero
// on this fleet. A shared helper rather than two call sites because the two
// documents drifting apart is the failure, not either one being wrong.
//
function currentCapsDocument(){
  return capsDocument(PLAY_CAPS, decodeLimits());
}
// What this browser was handed, and what it had said it could take.
//
// A stream rejection used to be reported as "browser refused the remux
// stream", which names the wrong party and drops the only two facts that
// separate a decoder that genuinely cannot keep up from a server that sent a
// profile this client never claimed. `PLAYER.deliveredDvProfile` is the
// server's own answer for what the session's bytes carry — it arrives on the
// create response — and `PLAY_CAPS.dvprofile` is the CSV this browser
// declared to `/decision`. When the first is not in the second, the browser
// did exactly what its capabilities document said it would.
//
// `caps_mismatch` is derived here for the viewer's sentence only. The server
// recomputes it from the two fields rather than believing this one, because a
// client asserting its own correctness is not evidence.
function streamRejectionFacts(){
  const p=(typeof PLAYER!=="undefined"&&PLAYER)||{};
  const declared=String((typeof PLAY_CAPS!=="undefined"&&PLAY_CAPS&&PLAY_CAPS.dvprofile)||"");
  const delivered=Number(p.deliveredDvProfile)>0?Number(p.deliveredDvProfile):null;
  // Compared as numbers, because the server does: it parses each element as an
  // integer, so a client comparing `"08"` as a string would send an accusatory
  // message on a line the server marks as agreeing.
  const claimed=declared.split(",").map(s=>parseInt(s,10)).filter(n=>Number.isFinite(n));
  return {
    delivered_range:p.deliveredRange||null,
    delivered_dv_profile:delivered,
    declared_dv_profiles:declared||null,
    caps_mismatch:!!(delivered&&claimed.length&&claimed.indexOf(delivered)===-1),
  };
}
// Whether the profile can plausibly be what broke this playback.
//
// `streamRejectionFacts()` reports a standing property of the SESSION, not a
// property of this error. A dropped link on a Dolby Vision session is still a
// mismatch and is still worth recording — but it is not what failed, and a
// report that says it is sends an operator after a capabilities bug that did
// not happen. Only a decoder-class failure can be the profile: on the
// `<video>` path that is `MEDIA_ERR_DECODE` (3) and `MEDIA_ERR_SRC_NOT_SUPPORTED`
// (4), and nothing else — the element path classifies *every* error as a media
// failure for the fallback decision, deliberately, which is a wider test than
// this one needs.
function rejectionBlamesTheProfile(facts, causeIsDecode){
  return !!(facts.caps_mismatch && causeIsDecode);
}
// One sentence for the operator's log, from both rejection paths. `cause` is
// whatever that path knows: the media element's error code and its name, or
// hls.js's `details`.
function streamRejectionMessage(facts, method, cause, causeIsDecode){
  const m=String(method||"").replace('_',' ').trim()||"copied";
  if(rejectionBlamesTheProfile(facts, causeIsDecode)){
    return "the server handed this browser a Dolby Vision Profile "
      +facts.delivered_dv_profile+" stream it did not declare (declared: "
      +facts.declared_dv_profiles+"; "+cause+") — re-encoding";
  }
  return "this browser could not decode the server's "+m+" stream ("+cause+") — re-encoding";
}
// The same finding, in the sentence the VIEWER reads.
//
// Separate from the log line because they are read by different people for
// different reasons, and identical in the one respect that matters: neither
// may say "the browser refused" when the browser did exactly what its own
// capabilities document promised. That wording is the whole complaint this
// milestone exists to answer, and leaving it in the two strings on screen
// while fixing the one nobody sees would have been the wrong half.
function streamRejectionNote(facts, method, cause, causeIsDecode){
  const m=String(method||"").replace('_',' ').trim()||"copied";
  if(rejectionBlamesTheProfile(facts, causeIsDecode)){
    return "the server sent a Dolby Vision Profile "+facts.delivered_dv_profile
      +" stream this browser never claimed ("+cause+") — re-encoding instead";
  }
  return "this browser could not decode the "+m+" stream the server sent ("
    +cause+") — re-encoding instead";
}
// One `stream_rejected` report, built the same way on both paths.
//
// Spelling the fields out at each call site is what left the only test of them
// a source grep: there was nothing to call. `base` is what that path alone
// knows — its own `code`/`src`/`detail` and the control trigger; the join and
// the Dolby Vision facts are added here, for both.
//
// `playbackContext()` is applied last on purpose. Its keys are disjoint from
// everything above, so nothing is overwritten — but `session` is the field the
// server needs to tie this report to the session it superseded, and a merge
// order that could drop it is the defect this whole report exists to fix.
function streamRejectionReport(facts, base){
  return Object.assign({level:"warn",event:"stream_rejected"}, base||{}, {
    delivered_range:facts.delivered_range,
    delivered_dv_profile:facts.delivered_dv_profile,
    declared_dv_profiles:facts.declared_dv_profiles,
  }, playbackContext());
}
// Whether a create should send it. An EMPTY document is worse than none at
// all: the server counts one as `unusable_caps` and returns no review, which
// is the pre-#842 hole with extra steps. `/decision` has no such arm — it has
// nothing to fall back to and answers on whatever it was given — so only the
// create asks this.
function capsDocumentIsUsable(doc){
  return !!doc && ((doc.video||[]).length>0 || (doc.audio||[]).length>0
    || (doc.containers||[]).length>0
    || (Object.prototype.hasOwnProperty.call(doc,"progressive_hevc_sample_entries")
      &&doc.progressive_hevc_sample_entries!==null));
}
// Re-probe through MediaCapabilities and rebuild the wire string. Async because
// decodingInfo is, and the server only ever hears one answer per session — so
// boot() awaits this before it can route anything, and every consumer reads
// CAPS_Q at call time rather than capturing it. A rejection anywhere leaves the
// synchronous answer standing, which is the conservative one.
const PLAY_CAPS_READY=(async()=>{
  try{
    const mc=await hevcTiersMediaCapabilities();
    if(mc){
      PLAY_HEVC_TIERS=mc.passed.slice();
      PLAY_HEVC_PQ_TIERS=mc.pqPassed.slice();
      PLAY_CAPS=buildPlayCaps(hevcTierSummary(mc.passed, mc.pqPassed));
    }
    const dvProfiles=String(PLAY_CAPS.dvprofile||"").split(",")
      .map(Number).filter(n=>n===5||n===8);
    PLAY_CAPS.progressiveHevcSampleEntries=
      await progressiveHevcSampleEntries(
        PLAY_HEVC_TIERS,PLAY_HEVC_PQ_TIERS,dvProfiles);
    CAPS_Q=capsQuery(PLAY_CAPS);
  }catch(e){}
  return PLAY_CAPS;
})();

// Manual quality override (player menu), persisted per browser. Auto runs the
// automatic ladder; Original never transcodes video (direct/remux only); a
// height forces a transcode at that size.
// "Original · one stream" is a TRANSPORT choice, not a quality one, and it is
// here because this menu is where a viewer already changes how a title is
// being delivered.
//
// Auto cuts every probed remux into fMP4 fragments and feeds them through
// MediaSource when this browser accepts the exact codec pair. The same remux
// as one continuous progressive response is a very different browser path,
// and the only way to compare them on the SAME title at the SAME bitrate is to
// ask for it explicitly. Without this entry, testing "is it the segmenting?"
// means finding a browser or codec that vetoes MSE and accepting two variables
// at once, which is how a whole day gets spent eliminating the wrong things.
const QUALITY_MODES=[["auto","Auto"],["original","Original"],
                     ["nomse","Original · one stream"]];
function playQuality(){ try{ return localStorage.getItem("plurx_quality")||"auto"; }catch(e){ return "auto"; } }
function qualityOptions(){
  const ladder=PlaybackPolicy.normalizedLadder(PLAYER&&PLAYER.ladder);
  return QUALITY_MODES.concat(ladder.slice().reverse().map(r=>[String(r.height),`${r.height}p`]));
}
function autoLastGoodHeight(){
  try{ const h=Number.parseInt(localStorage.getItem("plurx_auto_rung")||"",10); return h>0?h:null; }
  catch(e){ return null; }
}
function rememberAutoRung(height){
  if(!(height>0)) return;
  try{ localStorage.setItem("plurx_auto_rung",String(Math.round(height))); }catch(e){}
}
function playerPixelHeight(video){
  if(!video) return Infinity;
  try{
    const layoutHeight=video.getBoundingClientRect().height||video.clientHeight;
    return PlaybackPolicy.playerPixelHeight({
      layoutHeight,devicePixelRatio:window.devicePixelRatio,
      intrinsicHeight:video.videoHeight
    });
  }catch(e){ return Infinity; }
}
function qualityForce(){
  return PlaybackPolicy.qualityForce(playQuality());
}
// The viewer has asked for one continuous stream, so decline the segmented
// transport however attractive the server thinks it is.
function noSegments(){ return playQuality()==="nomse"; }
// The viewer's explicit rung, or null for Auto. Auto's opening rung is derived
// separately from the server ladder after /decision; keeping this helper about
// the manual choice stops a persisted Auto state from masquerading as a viewer
// request in the menu and routing logs.
function transcodeHeight(){ return PlaybackPolicy.transcodeHeight(playQuality()); }
// The height a transcode session should carry when the rung is not Auto's to
// pick, or null where it is. Two cases, one principle: the rung may not
// downgrade a resolution that was already promised.
//
// Original is that promise made explicitly, and it must survive the cases
// where a transcode happens anyway — a bitmap-subtitle burn, or the browser
// refusing the remux. Those sessions used to send no height at all, and the
// server's old Auto answered `min(source, 1080)`: a separate policy cap and
// the wrong answer for a viewer who explicitly asked for max, which is how a
// 4K film with forced PGS subs played at 1080p whatever the menu said.
//
// An AUTO burn over a remux/direct verdict is the same promise made by the
// server: the decision already chose to send this client the full-resolution
// stream, so the burn may add a re-encode but not a downscale. Genuine
// transcode verdicts still keep the server's Auto result rather than inheriting
// that promise. Auto resolves encoder/pipeline capability first — preserving a
// known SDR source on hardware — and lowers it only from stored network
// evidence. Bandwidth still agrees for this burn: the 4K rung's 20 Mb/s is
// *less* than the 57 Mb/s remux it replaces.
//
// A REFUSED original is that same promise again, and it was the case still
// getting downscaled. When the browser rejects a remux outright the client
// rescues it into a transcode — and that rescue was taking Auto's rung, so a
// 4K disc remux Chrome would not accept came back at 1080p with the overlay
// cheerfully reporting a remux verdict beside a 1080p picture. But the
// refusal is a statement about a CONTAINER or a codec (here: an fMP4 whose
// segments Chrome's demuxer will not parse), never about how many pixels the
// machine can push — and the server had already judged this client able to
// take the source. So the rescue adds the encode it must and keeps the
// resolution it was promised. Bandwidth agrees here too, and by more: 20 Mb/s
// of 4K rung against the 68 Mb/s remux it stands in for.
//
// Only that refusal. `maybeDecodeRescue` also lands in the same fallback, and
// it means the opposite thing — a machine that measured itself short of
// decode headroom — so it keeps the Auto cap, which is the entire point of
// rescuing it.
//
// The server still clamps ([144, 2160]) and never upscales; an unprobed
// source has no height to promise, so it stays the server's choice.
function sessionHeight(){
  const h=PLAYER&&PLAYER.source&&PLAYER.source.height;
  return PlaybackPolicy.sessionHeight({
    quality:playQuality(), sourceHeight:h,
    decidedMethod:PLAYER&&PLAYER.decidedMethod,
    burnedSubtitle:!!(PLAYER&&PLAYER.burnedSub!=null),
    refusedOriginal:!!(PLAYER&&PLAYER.refusedOriginal)
  });
}
function qualityLabel(){
  const q=playQuality(), f=qualityOptions().find(x=>x[0]===q);
  if(f) return f[1];
  const h=PlaybackPolicy.transcodeHeight(q);
  return h?`${h}p`:"Auto";
}

async function play(fileId, title, resumeMs, knownDurMs, meta, reservedOpenAttempt, retryIntent){
  const beginning=beginPlayAttempt(fileId,title,resumeMs,knownDurMs,meta,reservedOpenAttempt,retryIntent);
  const inputs=capturePlayInputs(fileId,meta,retryIntent);
  const attempt=Object.freeze({...beginning,...inputs,fileId,title,resumeMs,knownDurMs,meta});
  const {modal,playerWasOpen}=attempt;
  document.getElementById("playTitle").textContent=title;
  modal.classList.add("open");
  if(!playerWasOpen){
    const app=document.getElementById("app"); if(app) app.inert=!WATCH;
    const initial=document.getElementById("pbplay"); if(initial) initial.focus({preventScroll:true});
  }
  raisePlaybackSurface("client_preparing",{context:"start",
    title:"Reading media…",detail:"checking the file on the server"});
  const decided=await decideForPlay(attempt);
  if(!decided) return;
  const prepared=preparePlayOutgoing(attempt,decided.decision);
  const openedPlayer=PLAYER=buildPlayer(attempt,decided,prepared);
  const openIsAttached=()=>attempt.openIsCurrent()&&PLAYER===openedPlayer
    &&PLAYER.openToken===attempt.openAttempt.token;
  const initialAudio=presentPlayerChrome(attempt,decided,prepared,openIsAttached);
  const initialRoute=choosePlayRoute(attempt,decided,prepared,initialAudio);
  if(initialRoute==null) return;
  const attached=attachPlayRoute(attempt,decided,prepared,openedPlayer,openIsAttached,initialAudio,initialRoute);
  // Direct and progressive routes finish in this turn, as they did before
  // extraction; only session and copy-HLS routes wait for a server response.
  if(attached!==true && !await attached) return;
  finishPlayAttach(attempt,prepared,openedPlayer,openIsAttached);
}
function beginPlayAttempt(fileId,title,resumeMs,knownDurMs,meta,reservedOpenAttempt,retryIntent){
  WATCH_CLOSE_PROMISE=null;
  // Live TV holds a physical tuner and, since the dock, keeps holding it on
  // every other route. Starting a film used to be the moment it was released
  // (the route change stopped it); now the dock survives, so two pictures
  // play at once and the tuner is held behind a modal with no way to reach
  // its Stop button. `watchLiveTv` closes an open film for the same reason —
  // this is the missing half of that pair.
  if(typeof LIVE_TV_LEASE!=="undefined"&&(LIVE_TV_LEASE.current||LIVE_TV.starting)) stopLiveTv().catch(()=>{});
  const openAttempt=reservedOpenAttempt||PLAY_OPEN_GATE.begin("open");
  const libraryChannel=PENDING_LIBRARY_CHANNEL_PLAYBACK;
  PENDING_LIBRARY_CHANNEL_PLAYBACK=null;
  const openIsCurrent=()=>PLAY_OPEN_GATE.current(openAttempt);
  const predecessor=PLAYER?.mediaPredecessor||PLAYER;
  play.failedPreparation=null;
  const preparation=beginPlaybackPreparation(openIsCurrent);
  const failPreparation=(error,attempt)=>{
    if(!openIsCurrent()) return;
    preparation.finish();
    const wanted=PLAYER;
    const latest=wanted?.fileId===fileId;
    const retry={fileId,title,knownDurMs,meta,predecessor,
      resumeMs:latest&&wanted.controlSeek?Math.round(wanted.controlSeek.targetMs):resumeMs,
      wantsPlayback:fullIntent.wantsPlayback,
      selection:latest?playbackSelection(wanted,fileId):attempt.selection,
      audioOffsetMs:latest?(wanted.aoffset||0):attempt.sessionAudioOffset};
    play.failedPreparation=retry;
    if(play.pendingIntent===fullIntent)play.pendingIntent=null;
    if(predecessor){
      PLAYER=predecessor;
      if(latest&&predecessor.fileId===fileId){
        const audio=selectedAudioIndex(wanted);
        Object.assign(predecessor,{wantsPlayback:wanted.wantsPlayback,
          controlSeek:wanted.controlSeek,controlSeekSequence:wanted.controlSeekSequence,
          controlIntentGeneration:wanted.controlIntentGeneration,
          preplay:retry.selection,aoffset:retry.audioOffsetMs,
          curSub:wanted.curSub,burnedSub:wanted.burnedSub,pendingOpenAttempt:openAttempt});
        const index=predecessor.audio?.findIndex(track=>track.index===audio);
        if(index>=0)predecessor.curAudio=index;
      }
    }
    // A predecessor that is still ATTACHED makes this a refused CHANGE, not a
    // failed start (contract §3.3 row 7). Attached, not playing: a viewer who
    // paused before asking for a different quality still has that stream under
    // them, and covering it because the element happens to be paused is the
    // same mistake in a quieter spelling.
    const surfaceContext=(PLAYER&&PLAYER.started)?"change":"start";
    // The create-retry owner (M5) stops the player and raises `exhausted`
    // itself, on the clock, at the moment the deadline passes. Raising again
    // here would put a second fault of a lower class over its prompt.
    if(error&&error.surfaceRaised) return;
    if(!showSessionOpenFailure(error,surfaceContext)){
      if(surfaceContext==="change"){
        raisePlaybackSurface("change_failed",{context:"change",
          intent:playbackSurfaceIntent(PLAYER),
          title:"Playback could not prepare.",detail:error.message,actions:["retry"]});
      }else{
        stopPlayerForExhaustion();
        raisePlaybackSurface("owner_stopped",{context:"start",player_stopped:true,
          title:"Playback could not prepare.",detail:error.message,
          actions:["retry","close"]});
      }
    }
  };
  if(PLAYER){
    // A full play owns the destination immediately, including a different
    // title whose decision has not returned yet. Old partial creates and
    // decoder callbacks cannot attach over its loading phase.
    PLAYER._seekToken=(PLAYER._seekToken||0)+1;
    PLAYER.pendingMediaChange=null;
    PLAYER.mediaAttachment=null;
    PLAYER.retiringOpenAttempt=openAttempt;
  }
  if(predecessor)predecessor.retiringOpenAttempt=openAttempt;
  const inputPlayer=PLAYER&&PLAYER.fileId===fileId
    &&document.getElementById("modal")?.classList.contains("open")?PLAYER:null;
  if(inputPlayer){
    rememberPlaybackTransportIntent(document.getElementById("video"),inputPlayer);
    inputPlayer.pendingOpenAttempt=openAttempt;
  }
  const fullIntent={attempt:openAttempt,fileId,predecessor,
    wantsPlayback:retryIntent?retryIntent.wantsPlayback:(inputPlayer?inputPlayer.wantsPlayback:true)};
  play.pendingIntent=fullIntent;
  return {openAttempt,libraryChannel,openIsCurrent,predecessor,preparation,failPreparation,fullIntent};
}
function capturePlayInputs(fileId,meta,retryIntent){
  // Consume both one-shot inputs before the first await. An overlapping play
  // may set its own reason while this decision is in flight; reading the
  // shared slot afterwards would steal the newer attempt's attribution.
  const requestedAttemptReason=takePlaybackAttemptReason();
  // Consumed first, whatever happens to this attempt: a "subtitles off"
  // restart that failed at the decision must not leak its flag into the next
  // unrelated play and silently strip that one's default track.
  const skipDefaultSub=PENDING_DEFAULT_SUB_OFF; PENDING_DEFAULT_SUB_OFF=false;
  // An internal reopen (quality/subtitle change) is still this playback and
  // keeps its correction. A different title, or a player that was closed,
  // starts clean at zero.
  const sessionAudioOffset=retryIntent?.audioOffsetMs??
    (PLAYER&&PLAYER.fileId===fileId ? (PLAYER.aoffset||0) : 0);
  const replacementBandwidthSeed=PLAYER&&PLAYER.fileId===fileId
    ? PlaybackPolicy.bandwidthSeedBps({
        outgoingEstimateBps:PLAYER.hls&&PLAYER.hls.bandwidthEstimate,
        priorKbps:PLAYER.priorKbps})
    : null;
  const continuingPlayback=!!(PLAYER&&PLAYER.fileId===fileId);
  const replacementControlSeek=continuingPlayback&&PLAYER.controlSeek
    ? Object.assign({},PLAYER.controlSeek,{executed:false,frameFloor:0,audioPositionMs:null})
    : null;
  const replacementControlSequenceFloor=continuingPlayback
    ? Math.max(Number(PLAYER.controlSequenceFloor)||0,
        Number(PLAYER.controlReporter&&PLAYER.controlReporter.sequence)||0)
    : 0;
  const replacementControlIntentGeneration=continuingPlayback
    ? (PLAYER.controlIntentGeneration||0) : 0;
  // The track choice this playback runs with: the detail screen's pickers on a
  // cold start, what is already playing on an internal reopen.
  const selection=retryIntent?.selection??playbackSelection(PLAYER, fileId);
  const modal=document.getElementById("modal"), video=document.getElementById("video");
  const playerWasOpen=modal.classList.contains("open");
  const playerOpener=playerWasOpen?(PLAYER&&PLAYER._opener):document.activeElement;
  const playerOpenerClick=playerWasOpen?(PLAYER&&PLAYER._openerClick):
    (playerOpener&&playerOpener.getAttribute&&playerOpener.getAttribute("onclick"));
  const playerLastFocused=(PLAYER&&PLAYER._lastFocusedControl)||"pbplay";
  if(PLAYER){ PLAYER._opener=playerOpener; PLAYER._openerClick=playerOpenerClick; }
  wirePlayer();
  const watchTicket=watchPrepare(fileId,meta);
  // Stamp the click, not the decision: time-to-first-frame is what the person
  // waiting actually experiences, and it includes every server round trip
  // this function is about to make.
  const clickedAt=performance.now();
  return Object.freeze({requestedAttemptReason,skipDefaultSub,sessionAudioOffset,replacementBandwidthSeed,
    replacementControlSeek,replacementControlSequenceFloor,replacementControlIntentGeneration,
    selection,modal,video,playerWasOpen,playerOpener,playerOpenerClick,playerLastFocused,
    watchTicket,clickedAt});
}
async function decideForPlay(attempt){
  const {fileId,preparation,selection,openIsCurrent,failPreparation}=attempt;
  // The selection travels WITH the decision, so `method`, `reasons`, `delivery`
  // and the marked default tracks all describe the tracks that are about to
  // play. Asking for the policy default and correcting afterwards is what used
  // to force a remux over a perfectly decodable audio choice.
  let decision;
  try{ decision=await preparation.run(signal=>askDecision(fileId, qualityForce(), selection,signal));}
  catch(e){failPreparation(e,attempt);return null;}
  if(!openIsCurrent()) return null;
  let retestDecodeLimit=false;
  let learnedLimitView=null;
  // A device that measured this exact media load as unstable routes straight
  // to a transcode on Auto instead of stuttering mid-scene. The pure policy
  // rejects legacy codec/height entries and sibling HDR/bitrate identities;
  // Original is an explicit bypass, not another branch that can accidentally
  // inherit the remembered verdict.
  if(decision && (decision.method==='remux'||decision.method==='direct_play')){
    const learned=PlaybackPolicy.learnedDecodeLimitAction({
      quality:playQuality(),source:decision.source||{},limits:decodeLimits(),
      nowMs:Date.now(),ttlMs:DECODE_LIMIT_TTL_MS,retestMs:DECODE_LIMIT_RETEST_MS
    });
    const lim=learned.limit;
    // Past its re-test date: let this one session play the source and find
    // out, rather than trusting a week-old verdict forever.
    if(learned.action==='retest'){
      retestDecodeLimit=true;
      learnedLimitView=PlaybackPolicy.learnedDecodeLimitView({
        source:decision.source||{},limit:lim,
        ordinaryRange:decision.delivered_dynamic_range,
        deliveredRange:decision.delivered_dynamic_range
      });
      clientLog({level:"info",event:"decode_limit_retest",
        message:`re-testing ${decodeLimitLabel(decision.source||{})} on Auto — `+
                `the remembered measurement is over a week old`});
    } else if(learned.action==='apply'){
      try{
        const ordinaryRange=decision.delivered_dynamic_range;
        // Same selection: this replaces the decision above, so dropping it here
        // would hand the learned-limit transcode the policy default audio and
        // silently discard the viewer's choice on exactly the devices that need
        // the transcode most.
        const d2=await preparation.run(signal=>askDecision(fileId,"transcode",selection,signal));
        if(!openIsCurrent()) return null;
        learnedLimitView=PlaybackPolicy.learnedDecodeLimitView({
          source:decision.source||{},limit:lim,ordinaryRange,
          deliveredRange:d2.delivered_dynamic_range
        });
        d2.reasons=[`${learnedLimitView.reason} `+
          `(Quality → Original bypasses this measurement)`].concat(d2.reasons||[]);
        decision=d2;
      }catch(e){
        if(!openIsCurrent()) return null;
        if(e.name==='TimeoutError'||e.name==='AbortError'){failPreparation(e,attempt);return null;}
        /* the limit is advice; the original decision still plays */
      }
    }
  }
  return {decision,retestDecodeLimit,learnedLimitView};
}
function preparePlayOutgoing(attempt,decision){
  const {resumeMs,video,predecessor,meta,selection}=attempt;
  const startSec=(resumeMs||0)/1000;
  const ladder=decision.ladder||[];
  const priorKbps=decision.prior_kbps||null;
  const autoStartHeight=qualityForce()==='auto' && decision.method==='transcode'
    // The decision ladder does not know this node's grade-specific hardware
    // ceiling. Let the session endpoint choose its highest proved HDR10 rung
    // instead of allowing an old 720p SDR last-good value to cap it.
    && decision.delivered_dynamic_range!=='hdr10'
    ? PlaybackPolicy.initialAutoRung({ladder,persistedHeight:autoLastGoodHeight(),
        priorKbps,playerHeight:playerPixelHeight(video)})
    : null;
  // Keep the real outgoing decoder until preparation succeeds. Its callbacks
  // are fenced during preparation; retire that exact object (not the incoming
  // PLAYER) at attachment so its session and timers cannot be leaked.
  const outgoing=predecessor;
  let retired=false;
  const retireOutgoing=()=>{
    if(retired) return;retired=true;
    const incoming=PLAYER;
    if(outgoing){
      PLAYER=outgoing;
      stopPlayerTimers();
      teardownHls();
      if(outgoing.sessionId)releaseSession(outgoing.sessionId);
      PLAYER=incoming;
    }
    if(incoming)incoming.mediaPredecessor=null;
    clearSubs(video);
  };
  const src=decision.source||{};
  const book=meta&&meta.audiobook;
  // A pre-play subtitle is resolved BEFORE the player object exists, because a
  // burn has to ride the first session open — `transcodeOpts()` reads
  // `PLAYER.burnedSub`.
  const applied=prePlayApplication(decision, selection);
  const wantSub=applied.subtitle, preBurn=applied.burnedSub;
  return {startSec,ladder,priorKbps,autoStartHeight,retireOutgoing,src,book,
    applied,wantSub,preBurn,outgoing};
}
function buildPlayer(attempt,decided,prepared){
  const {decision,retestDecodeLimit,learnedLimitView}=decided;
  const {fileId,title,knownDurMs,meta,playerLastFocused,playerOpener,playerOpenerClick,
    libraryChannel,sessionAudioOffset,replacementBandwidthSeed,replacementControlSeek,
    replacementControlIntentGeneration,replacementControlSequenceFloor,clickedAt,
    openAttempt,fullIntent,selection}=attempt;
  const {src,book,preBurn,ladder,priorKbps,autoStartHeight,outgoing}=prepared;
  return {fileId, timer:null, offset:0, hls:null, knownDur:knownDurMs||0,
    decodeRetest:retestDecodeLimit,
    durMs:(src.duration_ms||knownDurMs||0), method:decision.method, encoder:null,
    capsSnapshot:decision._capsSnapshot||currentCapsDocument(),
    requiresHls:!!(decision.delivery&&decision.delivery.requires_hls),
    // `method` mutates as the session moves (a burn sets it to 'transcode');
    // this keeps the DECISION's verdict, which is what sessionHeight() needs
    // to tell a burn-over-remux from a burn on a genuine transcode.
    decidedMethod:decision.method,
    // `default` on both arrays is the EFFECTIVE selection, not the container's
    // flag: the server marks whatever it selected, including the index this
    // request asked for. So a pre-play audio choice is already the checked one.
    audio:decision.audio||[], subs:(decision.subtitles||[]),
    curAudio:(decision.audio||[]).findIndex(a=>a.default), curSub:preBurn!=null?preBurn:-1,
    // This playback's track choice, carried across an internal reopen and kept
    // current by the in-player Audio and Subtitles menus.
    preplay:selection||null,
    markers:(decision.markers||[]).map(m=>({...m})), source:src, started:false,
    _markerOffers:new Set(),
    _seekPreview:null, _seekPending:null, _lastFocusedControl:playerLastFocused,
    _opener:playerOpener, _openerClick:playerOpenerClick,
    idleTimer:null, autoskip:libraryChannel?false:autoskipOn(), stallTimer:null, probeUrl:null, directUrl:null,
    triedFallback:false,
    mediaRecoveries:0,
    mediaRecoveredAtMs:null,
    refusedOriginal:false,
    aoffset:sessionAudioOffset, declared:decision.declared_offset_ms||0,
    reasons:(decision.reasons||[]), title, meta:meta||null, decodeRescued:false,
    // The one automatic session-open claim; see claimAutoFallback.
    autoFallbackInFlight:false,
    learnedLimit:learnedLimitView,
    bookOffset:book?(meta.part_offset_ms||0):0,
    bookDuration:book?(meta.book_duration_ms||0):0,
    bookParts:book?(meta.book_parts||[]):null,
    transcodeAudio:!!decision.transcode_audio,
    preserveDolbyVision:!!decision.preserve_dolby_vision, copyHls:false,
    // What `/decision` asked the session builder to preserve. Unlike
    // deliveredRange this never changes when a session response reports what
    // it actually produced, so seek/audio/quality reopens retain the grade.
    requestHdr10:decision.method==='transcode'&&decision.delivered_dynamic_range==='hdr10',
    // What grade the DECISION plans to put on the wire. A session overrides it
    // the moment one attaches (a burn or a forced rung transcodes something the
    // decision never promised); direct play has no session, so this stands for
    // the whole playback. MEDIA-BADGES-PLAN.md §3.2.
    deliveredRange:decision.delivered_dynamic_range||null,
    deliveredDvProfile:decision.delivered_dolby_vision_profile||null,
    playStartedAt:clickedAt, ttffMs:null, stalls:0, sessionId:null, streamId:null, health:null,
    attemptId:null, attemptReason:null, bufferLimits:null,
    ladder, priorKbps, autoHeight:autoStartHeight,
    bandwidthSeedBps:replacementBandwidthSeed,
    abr:{lastSwitchAtMs:clickedAt,lastStallAtMs:null,mildSamples:0,
      upgradeSinceMs:null,previousRunway:null,stallEvents:{supply:[],decode:[]},
      recentEstimateKbps:null,recentEstimateAtMs:null,
      recentEstimateSource:null,recentEstimateUrl:null,
      switches:[],switching:false,stableSinceMs:clickedAt,supplyRescued:false,
      // Rungs this playback has already failed to open. Per playback, not
      // persisted: a transient server failure must not cap quality forever.
      failedHeights:new Set()},
    waitAt:null, waitStartedRunway:null, waitTimer:null, waitReported:false,
    stallsByKind:null, stallRecoveries:0, recoveringStall:null,
    // The presenter's state, shared by every player object this page has: the
    // overlay is one element with one history and identity is the generation
    // each fault carries (PLAYBACK-SURFACE-CONTRACT.md §3.1).
    surfaceState:PLAYBACK_SURFACE,
    burnedSub:preBurn, vod:false, openToken:openAttempt.token,
    controlSeek:replacementControlSeek,
    controlSeekSequence:replacementControlSeek?replacementControlSeek.sequence:0,
    controlIntentGeneration:replacementControlIntentGeneration,
    controlSequenceFloor:replacementControlSequenceFloor,
    controlPresentedFrames:0,controlHasFrameCallbacks:false,
    wantsPlayback:fullIntent.wantsPlayback,
    mediaPredecessor:outgoing,
    internalMediaReset:true,pendingOpenAttempt:openAttempt,libraryChannel,
    terminalStop:null, };
}
function presentPlayerChrome(attempt,decided,prepared,openIsAttached){
  const {decision}=decided;
  const {requestedAttemptReason,skipDefaultSub,video,meta,libraryChannel,
    playerWasOpen,openAttempt}=attempt;
  const {startSec,applied,wantSub,book}=prepared;
  newAttempt(requestedAttemptReason || (startSec>0?"resume":"cold-start"));
  // §3.3 row 17: the picture the viewer is getting is not quite the one they
  // asked for, and that is a notice beside it. Raised here rather than where
  // the decision was read, because a fault is about a media generation and
  // this is the line that mints the one this refusal belongs to.
  if(applied.blockedByHdr) raisePlaybackSurface("degraded_notice",
    {title:"That subtitle requires an SDR burn-in. HDR playback was kept unchanged."});
  if(PLAYER.curAudio<0) PLAYER.curAudio=0;
  const initialAudio=selectedAudioIndex(PLAYER);
  setupAirplay(video);
  const player=document.getElementById("player");
  if(player){
    player.classList.toggle("audio",!!book);
    player.classList.toggle("lc-following",!!libraryChannel);
    player.style.backgroundImage=book&&meta.poster?`linear-gradient(rgba(0,0,0,.28),rgba(0,0,0,.72)),url(${JSON.stringify(tok(meta.poster)).slice(1,-1)})`:"";
  }
  setupTrackMenus();
  renderPlayerInfo();
  renderSkip(null);
  const sb=document.getElementById("skipbtn"); if(sb){ sb.classList.toggle("on", PLAYER.autoskip); sb.style.display=book?"none":""; }
  const ab=document.getElementById("autonextbtn"); if(ab){ ab.classList.toggle("on", autoNextOn()); ab.style.display=book?"none":""; }
  // Server-chosen default subtitle (Settings → Playback defaults / anime rule).
  // Native <track> subs ride direct/remux only; transcode burns them instead.
  //
  // A pre-play choice owns this instead. Off (`-1`) means leave it off; a burn
  // is already in `burnedSub` above and rides the first session open; a text
  // track is applied below, after the route knows its offset, so it lands
  // without the 400 ms guess and in every delivery method rather than only the
  // two the cold-start default is limited to.
  const defSub=PLAYER.subs.find(s=>s.default);
  if(defSub && !skipDefaultSub && wantSub==null && decision.method!=='transcode'){
    setTimeout(()=>{
      if(openIsAttached()&&PLAYER.curSub<0&&PLAYER.preplay?.subtitle==null) setSub(defSub.index);
    }, 400);
  }
  return initialAudio;
}
function choosePlayRoute(attempt,decided,prepared,initialAudio){
  const {decision}=decided;
  const {video,sessionAudioOffset,libraryChannel,failPreparation}=attempt;
  const {preBurn}=prepared;
  const nativeHls=useNativeHls(video);
  const segmentedRemux=segmentedRemuxOk(decision,video);
  const hlsAvailable=!noSegments()&&(nativeHls||copyHlsMseOk(decision,video));
  let initialRoute=playbackInitialRoute(
    decision,initialAudio,nativeHls,segmentedRemux,sessionAudioOffset,hlsAvailable);
  // Following always uses the finite-HLS session boundary where its durable
  // purpose and history isolation are enforced. Copy-HLS retains source video
  // when the decision does not require an encode.
  if(libraryChannel) initialRoute=decision.method==='transcode'?'transcode_hls':'copy_hls';
  // A burn is a transcode whatever the plan said. The server's plan is a remux
  // (or a direct play) when the PGS application overlay is enabled — a delivery
  // this player does not implement, so `prePlayBurnNeeded()` said yes where the
  // plan said no. Following the plan there would start a stream that cannot
  // show the chosen track and replace it a moment later, which is the re-buffer
  // a pre-play choice exists to avoid.
  if(preBurn!=null && initialRoute!=='transcode_hls'){
    initialRoute='transcode_hls';
    PLAYER.method='transcode'; PLAYER.copyHls=false;
    PLAYER.reasons=[`subtitle burn-in (${subLabelFor(PLAYER.subs.find(s=>s.index===preBurn),preBurn)}) `+
                    `— drawing subtitles into the picture forces a re-encode`].concat(PLAYER.reasons||[]);
  }
  if(initialRoute==='unsupported_hevc_delivery'){
    failPreparation(Object.assign(
      new Error("This browser cannot execute the required HEVC HLS delivery. Disable one-stream mode or use a browser with HLS support."),
      {code:"unsupported_hevc_delivery"}),attempt);
    return null;
  }
  prepared.nativeHls=nativeHls;
  return initialRoute;
}
function attachPlayRoute(attempt,decided,prepared,openedPlayer,openIsAttached,initialAudio,initialRoute){
  const {decision}=decided;
  const {fileId,video,openAttempt,preparation,failPreparation}=attempt;
  const {startSec,retireOutgoing}=prepared;
  const nativeHls=prepared.nativeHls;
  if(initialRoute==='transcode_hls') return (async()=>{
    // Transcode: start an HLS session (server re-encodes + tone-maps).
    // Read from PLAYER, not the decision: they hold the same array on the
    // ordinary path, and on the forced-burn path above PLAYER is the one that
    // knows a burn is why this is a transcode at all.
    raisePlaybackSurface("client_preparing",{context:"start",
      title:"Starting the transcoder…",detail:transcodeReason(PLAYER)});
    const options=transcodeOpts(startSec,initialAudio);
    const retryContext=playbackCreateRetryContext();
    let info; try{info=await preparation.run(
      signal=>openSessionRetryingNotYet(fileId,options,signal,{context:retryContext,preparation}),
      late=>releaseSession(late&&late.session_id));}catch(e){
      if(openIsAttached())failPreparation(e,attempt);
      return false;
    }
    if(!PLAY_OPEN_GATE.acceptResource(openAttempt,info&&info.session_id,releaseSession)) return false;
    if(!openIsAttached()){
      releaseSession(info&&info.session_id);
      return false;
    }
    retireOutgoing();
    const from=attachSession(video, openedPlayer, info, startSec);
    raisePlaybackSurface("client_preparing",{context:"start",title:"Preparing the stream…",
      detail:(PLAYER.encoder?("encoder: "+PLAYER.encoder+" — "):"")+
        (PLAYER.vod?"already transcoded — playing from the cache":"buffering the first segments")});
    armStall(from);
    return true;
  })();
  if(initialRoute==='copy_hls') return (async()=>{
    // Two different reasons to land here, same destination.
    //
    // Safari can't play a progressive fragmented-MP4 remux at all, but it plays
    // the same copy-video stream (source video kept, audio transcoded only) via
    // native HLS — routing it here keeps the original resolution instead of the
    // error-fallback re-encoding to 720p.
    //
    // Chrome *can* play the progressive stream, and on a big file it plays it
    // badly: its read-ahead there is a hard ~2.2 s that no response header can
    // raise (PERF-PLAN §4.3bis, measured), so any supply gap over two seconds
    // is a stall. The same bytes through MSE let hls.js hold a real buffer.
    // Identical video either way — this is a transport choice, not a rung.
    const why=!nativeHls && decision.prefer_segmented;
    raisePlaybackSurface("client_preparing",{context:"start",title:"Preparing the stream…",
      detail:why? `${why} — sending it as HLS so the player can buffer ahead`
                : "remuxing to HLS — keeping the original video"});
    try{ if(!await startCopyHls(video,startSec,openIsAttached,openAttempt,preparation,retireOutgoing)) return false; }
    catch(e){
      if(openIsAttached())failPreparation(e,attempt);
      return false;
    }
    return true;
  })();
  {
    // Direct play, or remux for a browser that plays progressive fMP4 (Chrome).
    // A raw direct-play file gives the browser control of its mux defaults, not
    // the server's language choice. If the preferred track is not stream zero,
    // use the remuxer so the requested track is the only one delivered.
    const isRemux = initialRoute==='progressive_remux';
    raisePlaybackSurface("client_preparing",{context:"start",
      title:isRemux?"Preparing the stream…":"Loading…",
      detail:isRemux?"remuxing to a browser-friendly container":"direct play — no conversion needed"});
    // Remux carries the same caps so the server copies the audio when the
    // browser can play it (Safari + AC-3) instead of re-encoding to AAC, and a
    // stream handle so the overlay can ask how it's going — a progressive
    // remux has no session, which is why the Server section used to be missing
    // on Chrome entirely.
    let url;
    if(isRemux){
      if(decision.method==='direct_play') PLAYER.method='remux';
      PLAYER.offset=startSec;
      const base=decision.method==='direct_play'
        ? `/api/v1/files/${fileId}/stream.mp4`
        : decision.play_url;
      url=remuxUrl(base, initialAudio, startSec);
      PLAYER.probeUrl=decision.play_url; armStall(startSec);
    } else {
      url=tok(decision.play_url);
      PLAYER.directUrl=url;
      PLAYER.probeUrl=decision.play_url;
      armStall(startSec);
    }
    retireOutgoing();
    resetMediaSource(video);
    const attachment=beginPlaybackMediaAttachment(openedPlayer);
    setPlaybackMediaSource(video,url);
    markPlaybackControlSeekExecuted(PLAYER,startSec);
    video.onloadedmetadata=()=>{
      if(!openIsAttached()) return;
      video.onloadedmetadata=null;
      if(!isRemux)
        applyPlaybackAttachmentPosition(video,openedPlayer,attachment,startSec); };
    applyPlaybackTransportIntent(video,openedPlayer);
  }
  return true;
}
function finishPlayAttach(attempt,prepared,openedPlayer,openIsAttached){
  const {watchTicket,preparation,fullIntent,video,fileId}=attempt;
  const {applied}=prepared;
  if(!openIsAttached()) return;
  openedPlayer.pendingOpenAttempt=null;
  watchAccept(watchTicket,openedPlayer);
  samplePlaybackPresentationClock(video,openedPlayer);
  notifyPlaybackControl();
  preparation.finish();
  if(play.pendingIntent===fullIntent)play.pendingIntent=null;

  // A pre-play TEXT subtitle, applied to the stream that just attached. Here
  // rather than before the route, because setSub() shifts the cues by
  // `PLAYER.offset` and the route is what sets it. A <track> is not a stream
  // restart, so the choice arrives with the first frames.
  if(applied.textSub!=null && openIsAttached()) setSub(applied.textSub);

  if(!openIsAttached()) return;

  // Once, per playback, as soon as the element knows what it is decoding.
  probeDecode();
  video.addEventListener("loadedmetadata", probeDecode, {once:true});
  armHitchDetector(video);
  armPlaybackSampling(video,openedPlayer);
  video.onended=()=>{ handleEnded(fileId).catch(()=>{}); };
  pbTick(); pbSyncPlayIcon();   // seed the transport (total time, play icon) at once
  playerActivity();
}
// Audio correction needs server muxing even if the file itself is directly
// playable. Re-reading /decision must not undo that delivery requirement.
function playbackInitialRoute(decision,audio,nativeHls,segmentedRemux,audioOffsetMs,hlsAvailable){
  return PlaybackPolicy.initialRoute({
    method:audioOffsetMs&&decision.method==='direct_play'?'remux':decision.method,
    selectedAudioIndex:audio,nativeHls,segmentedRemux,
    requiresHls:!!(decision.delivery&&decision.delivery.requires_hls),hlsAvailable
  });
}
// Commands arriving while a full open is awaiting a decision/session replace
// that open with one snapshot of the newest destination and track choices.
// Otherwise its late response would overwrite in-place mutations of PLAYER.
function restartPendingPlaybackOpen(p,reason){
  // Controls still describe the outgoing title until the new decision builds
  // its player. They must not reopen that title over the newer destination.
  if(p&&p.retiringOpenAttempt&&p.retiringOpenAttempt!==p.pendingOpenAttempt
     &&PLAY_OPEN_GATE.current(p.retiringOpenAttempt)) return true;
  if(p&&PLAYER===p&&p.pendingMediaChange){
    executePlaybackMediaChange(p,p.pendingMediaChange);
    return true;
  }
  if(!p||PLAYER!==p||!p.pendingOpenAttempt||!PLAY_OPEN_GATE.current(p.pendingOpenAttempt)) return false;
  PENDING_ATTEMPT_REASON=reason;
  const position=positionForPlaybackIntent(document.getElementById("video"),p);
  play(p.fileId,p.title||"",Math.round(position*1000),p.knownDur||0,p.meta);
  return true;
}
// Desired media is not the media currently attached. Keep the entire pending
// route until it attaches; a newer seek/track command inherits it even on VOD.
// Each execution snapshots options before its await and owns its own response.
function requestPlaybackMediaChange(p,change){
  // Track and quality changes replace the attachment that owned any pending
  // keyboard preview. Cancel it so a late keyup cannot seek the successor.
  if(p===PLAYER) cancelPendingSeek();
  p.pendingMediaChange=Object.assign({},p.pendingMediaChange||{},change);
  return executePlaybackMediaChange(p,p.pendingMediaChange);
}
async function executePlaybackMediaChange(p,change){
  const v=document.getElementById("video");
  if(!v||PLAYER!==p||p.pendingMediaChange!==change) return false;
  let changeKey=null;
  const pos=positionForPlaybackIntent(v,p), audio=selectedAudioIndex(p), subtitle=p.curSub;
  // What this execution *is*, recorded before its create opens so an
  // identical command arriving while it is open can be suppressed instead of
  // superseding it. Cleared on every exit below, the refusal path included,
  // so an explicit Retry is never mistaken for the attempt it retries.
  changeKey=playbackChangeRecipeKey(p,pos,change);
  const {live:streamIsCurrent}=streamGeneration();
  const live=()=>streamIsCurrent()&&p.pendingMediaChange===change;
  const preparation=beginPlaybackPreparation(live);
  const method=p.burnedSub!=null?'transcode':change.method;
  const opts=method==='transcode'?PlaybackPolicy.stallReopenSessionOptions({
    options:transcodeOpts(pos,audio,change.height),forceReopen:!!change.forceReopen,
    method,sessionId:change.previousSessionId||p.sessionId,
    cause:change.recoveryCause||"unknown"}):null;
  newAttempt(change.reason||"stream-change");
  p.stallFrom=pos;
  if(change.automatic&&p.abr) p.abr.switching=true;
  // Keep the predecessor until the create succeeds. Its output cannot settle
  // the new destination, which remains unexecuted until the attach below.
  raisePlaybackSurface("client_preparing",{
    title:"Preparing the stream…",detail:"your place and selections are saved"});
  try{
    // Set inside the `try` whose `finally` clears it: a throw between here
    // and there would otherwise leave the key set with nothing to clear it,
    // and every identical seek on this player would be silently swallowed
    // for the rest of the playback.
    p.inFlightChangeKey=changeKey;
    if(method==='transcode'){
      // The same door as the copy-HLS branch below. In a pending change this
      // wrapper is a passthrough — `playbackCreateRetryContext()` answers
      // `change`, row 7 beats row 6 and nothing is retried — and that is the
      // point: which context a create is in is decided in ONE place, not by
      // which branch of this function happened to call which function.
      const info=await preparation.run(
        signal=>openSessionRetryingNotYet(p.fileId,opts,signal,{preparation}),
        late=>releaseSession(late&&late.session_id));
      if(!live()){ releaseSession(info&&info.session_id); return false; }
      retirePlaybackPredecessor(p);
      teardownHls(); resetMediaSource(v); clearSubs(v);
      p.started=false; p.method='transcode'; p.copyHls=false;
      p.health=null;
      if(change.height>0) p.autoHeight=change.height;
      if(change.note) p.rescuedNote=change.note;
      armStall(attachSession(v,p,info,pos),PlaybackPolicy.HLS_STARTUP.seek_deadline_ms);
    }else if(change.copyHls){
      if(!await startCopyHls(v,pos,live,null,preparation,()=>retirePlaybackPredecessor(p))) return false;
    }else{
      retirePlaybackPredecessor(p);
      teardownHls(); resetMediaSource(v); clearSubs(v);
      p.started=false; p.method='remux'; p.copyHls=false; p.offset=pos;
      p.probeUrl=`/api/v1/files/${p.fileId}/stream.mp4`;
      setPlaybackMediaSource(v,remuxUrl(`/api/v1/files/${p.fileId}/stream.mp4`,audio,pos));
      markPlaybackControlSeekExecuted(p,pos);
      armStall(pos,20000); applyPlaybackTransportIntent(v,p);
    }
    if(!live()) return false;
    p.pendingMediaChange=null;
    samplePlaybackPresentationClock(v,p);
    notifyPlaybackControl();
    if(change.automatic&&p.abr){
      const now=performance.now();
      Object.assign(p.abr,{switching:false,lastSwitchAtMs:now,stableSinceMs:now,
        mildSamples:0,upgradeSinceMs:null,previousRunway:null});
      p.abr.stallEvents.supply=[];
      if(change.from!=null) recordAutoSwitch(p,change.from,change.height||"transcode Auto",
        change.switchReason||change.reason,pos,p.sessionId);
    }
    if(subtitle>=0&&p.burnedSub==null) await setSub(subtitle);
    return true;
  }catch(e){
    if(!live()) return false;
    if(change.automatic&&p.abr){
      p.abr.switching=false;
      if(change.height>0){
        p.abr.failedHeights=p.abr.failedHeights||new Set();
        p.abr.failedHeights.add(change.height);
      }
    }
    // Retain the desired recipe for Retry/the next command, but surface the
    // failed attempt now; neither silence nor restoring desired=attached lies.
    // The predecessor is deliberately still playing (that is what the retained
    // `pendingMediaChange` above means), so this is a refusal about the
    // destination — a notice beside the picture, never a screen over it. That
    // sticky "could not reconnect" over a running stream is §2.1's second
    // defect, and this line is where it lived.
    if(!showSessionOpenFailure(e,"change")){
      raisePlaybackSurface("change_failed",{context:"change",
        intent:playbackSurfaceIntent(p),
        title:"Playback could not reconnect.",detail:e.message,actions:["retry"]});
      toast(e.message);
    }
    // The refused destination is detached from the incumbent at this instant.
    // Say so now rather than letting the server's startup actor learn it on
    // the next scheduled exchange: the snapshot it gets carries the
    // incumbent's own presentation with no foreign target in it, which is
    // what stops a refusal from expiring a healthy stream.
    notifyPlaybackControl();
    return false;
  }finally{
    preparation.finish();
    if(p.inFlightChangeKey===changeKey) p.inFlightChangeKey=null;
  }
}
// Open (or re-open) a copy-video HLS session for the current file and attach it
// as native HLS. The video stream is the server's untouched original; only the
// audio is transcoded when the browser can't take it. Keeps method === 'remux'
// (honest: no video re-encode) and flags copyHls so seek / audio-switch re-open
// the HLS session instead of the progressive /stream.mp4 Safari can't play.
// `live` (optional) is re-checked after the await: a seek or audio switch that
// has since been superseded must not attach, or two hls.js instances end up
// pulling two server sessions at once. Returns false when it bailed.
async function startCopyHls(v, startSec, live, openAttempt, preparation, beforeAttach){
  const aidx=selectedAudioIndex(PLAYER);
  const src=PLAYER.source||{}, track=PLAYER.audio&&PLAYER.audio[PLAYER.curAudio];
  const codec=track&&track.codec;
  const hevcCopy=["hevc","h265","hevc10"].includes(String(src.video_codec||"").toLowerCase());
  const native=PlaybackPolicy.hlsTransport({
    nativeHls:useNativeHls(v), hevcCopy, hlsJsSupported:hlsJsSupported()
  })==='native';
  // `decision.transcode_audio` describes only the initially selected track.
  // Audio switches must ask again: widening copy-HLS to ordinary remuxes made
  // an AAC-default/AC-3-alternate file feed AC-3 into Chrome MSE and fail at
  // bufferAddCodecError. The copy session can convert just that track to AAC.
  const aac=PlaybackPolicy.copyAudioNeedsTranscode({
    codec, clientAudioCodecs:String(PLAY_CAPS.acodec||"").split(",").filter(Boolean),
    msePairSupported:mseCanTake(src.video_codec,codec,src.bit_depth), nativeHls:native
  });
  let info;
  try{
    const fileId=PLAYER.fileId, options={copy:true,aac,
      preserve_dolby_vision:!!PLAYER.preserveDolbyVision,start:startSec||0,audio:aidx,
      audio_offset_ms:PLAYER.aoffset||0};
    // `vod_index_pending` has a better answer than waiting on this path — the
    // progressive remux below — so it is answered locally instead of retried.
    const retry={context:playbackCreateRetryContext(),preparation,
      answeredLocally:failure=>PlaybackPolicy.indexPendingFallback({
        code:failure&&failure.code,method:PLAYER&&PLAYER.method,nativeHls:native,
        requiresHls:!!(PLAYER&&PLAYER.requiresHls)
      })==="progressive_remux"};
    // Both shipped call sites pass a preparation owner, and this is the only
    // thing that bounds and CANCELS an open — a create with no signal cannot
    // be cancelled by a newer intent at all, and M5's sequence would keep
    // posting behind one. Refuse it rather than leave the hole for the next
    // caller to find.
    if(!preparation) throw Object.assign(
      new Error("A copy-HLS open needs a preparation owner to bound and cancel it."),
      {name:"TypeError"});
    info=await preparation.run(
      signal=>openSessionRetryingNotYet(fileId,options,signal,retry),
      late=>releaseSession(late&&late.session_id));
  }catch(error){
    if(live && !live()) return false;
    const fallback=PlaybackPolicy.indexPendingFallback({
      code:error&&error.code,method:PLAYER&&PLAYER.method,nativeHls:native,
      requiresHls:!!(PLAYER&&PLAYER.requiresHls)
    });
    if(fallback!=="progressive_remux") throw error;
    // The immutable index is node-local and may still be catching up after a
    // deploy. This exact browser already proved it can take the progressive
    // remux; use that route instead of turning a background-cache miss into an
    // unplayable title. Safari/native HLS is excluded by the policy above.
    const from=startSec||0;
    if(beforeAttach)beforeAttach();
    const base=`/api/v1/files/${PLAYER.fileId}/stream.mp4`;
    PLAYER.method='remux'; PLAYER.copyHls=false; PLAYER.offset=from;
    PLAYER.probeUrl=base;
    clearStreamFailure();
    clientLog(Object.assign({level:"warn",event:"stream_route_fallback",
      detail:"vod_index_pending",
      message:"fragment index is pending — using one continuous remux"},playbackContext()));
    raisePlaybackSurface("client_preparing",{
      title:"Preparing one continuous stream…",
      detail:"the HLS index is still building — remuxing without segmentation"});
    resetMediaSource(v);
    if(live && !live()) return false;
    setPlaybackMediaSource(v,remuxUrl(base,aidx,from));
    markPlaybackControlSeekExecuted(PLAYER,from);
    armStall(from,20000); applyPlaybackTransportIntent(v,PLAYER);
    return true;
  }
  if(openAttempt&&!PLAY_OPEN_GATE.acceptResource(
    openAttempt,info&&info.session_id,releaseSession)) return false;
  if(live && !live()){
    releaseSession(info&&info.session_id);
    return false;
  }
  PLAYER.method='remux'; PLAYER.copyHls=true;
  if(beforeAttach)beforeAttach();
  armStall(attachSession(v, PLAYER, info, startSec));
  return true;
}
// One generation counter for "restart the server-side stream". Seek and audio
// switch both bump it, so they supersede each other as well as themselves —
// they are the same operation (re-open at position X with track Y) and only the
// last one asked for should end up attached.
function streamGeneration(){
  const me=PLAYER; const token=(me._seekToken=(me._seekToken||0)+1);
  if(me.pendingOpenAttempt&&PLAY_OPEN_GATE.current(me.pendingOpenAttempt)){
    PLAY_OPEN_GATE.invalidate();
    me.pendingOpenAttempt=null;
  }
  const intent=me.controlSeekSequence||0;
  return {me, live:()=>PLAYER===me && me._seekToken===token
    &&(me.controlSeekSequence||0)===intent};
}
function transcodeReason(d){
  const r=(d.reasons||[]).filter(Boolean);
  return r.length? r.join(" · ") : "building a compatible stream — this can take a few seconds";
}
function setupAirplay(video){
  const btn=document.getElementById("apbtn");
  if(!btn) return;
  if(window.WebKitPlaybackTargetAvailabilityEvent){
    video.addEventListener("webkitplaybacktargetavailabilitychanged",e=>{
      btn.style.display = e.availability==="available"?"":"none";
    });
    btn.onclick=()=>{ try{ video.webkitShowPlaybackTargetPicker(); }catch(e){} };
  } else { btn.style.display="none"; }
}
// Codec strings to try through MediaSource, per source codec. Several per
// codec because the profile/level in the string is checked and we do not know
// the source's: Chrome answers "no" to a level it cannot decode, so asking
// only about L153 (4K) would reject a 1080p file it would have played fine.
// Any accepted variant proves the *decoder* exists, which is the question.
//
// The HEVC ladders are split by bit depth because the profile in the string is
// the ONLY place a decoder's answer distinguishes them: `hvc1.1.6.*` is Main
// (8-bit) and `hvc1.2.4.*` is Main10. A browser with an 8-bit-only HEVC
// decoder says yes to the Main strings and no to the Main10 ones — so a ladder
// containing both, asked with `.some()`, passes a 10-bit source on the
// strength of an 8-bit yes. That is a stream MediaSource accepts and the
// decoder then renders as nothing: black picture, working sound, no error.
const MSE_VIDEO = {
  hevc:["hvc1.2.4.L153.B0","hvc1.1.6.L153.B0","hvc1.2.4.L120.B0","hvc1.1.6.L120.B0"],
  h265:["hvc1.2.4.L153.B0","hvc1.1.6.L153.B0","hvc1.2.4.L120.B0","hvc1.1.6.L120.B0"],
  // Main10 only. Selected by bit depth, not by codec name — see mseCodecKey.
  hevc10:["hvc1.2.4.L153.B0","hvc1.2.4.L120.B0"],
  h264:["avc1.640033","avc1.640029","avc1.4d401f"],
  avc:["avc1.640033","avc1.640029","avc1.4d401f"],
  av1:["av01.0.09M.10","av01.0.08M.10","av01.0.05M.08"],
  vp9:["vp09.02.51.10","vp09.00.41.08","vp09.00.10.08"],
};
const MSE_AUDIO = {
  aac:"mp4a.40.2", ac3:"ac-3", eac3:"ec-3", opus:"opus", flac:"flac", mp3:"mp4a.40.34",
};
// Which ladder describes THIS source.
//
// `video_codec` is ffprobe's `codec_name`, which is always plain "hevc" — it
// never carries the profile, so the `hevc10` ladder above was unreachable and
// every 10-bit file was asked about with the 8-bit strings in the list. The
// depth is on the wire (`SourceSummary.bit_depth`) and was read by the badges
// and by nothing that made a decision. It is read here now.
//
// An absent depth keeps today's ladder deliberately: unknown is not 10-bit,
// and guessing upward would push 8-bit sources off MSE on browsers that have
// always played them.
function mseCodecKey(srcCodec, bitDepth){
  const c=String(srcCodec||"").toLowerCase();
  const d=Number(bitDepth)||0;
  if(d>=10 && (c==="hevc"||c==="h265"||c==="hevc10")) return "hevc10";
  return c;
}
// Will this browser take the copy-video stream through MediaSource?
//
// Asked because `<video src>` and MSE are different code paths with different
// answers: Chrome plays plenty progressively that MediaSource refuses, and
// routing a remux to hls.js on the strength of the first would turn a stuttery
// stream into a black one. Video and audio are asked about together, in one
// string, because that is exactly what hls.js will ask — checking them apart
// passes pairs the browser will not accept as a pair.
//
// This picks the TRANSPORT, and nothing else: the same bytes either way, and
// nothing here is ever told to the server. A no sends the remux down the
// progressive path, which is where the `<video>` error handler's rescue lives.
function mseCanTake(srcCodec, audioCodec, bitDepth){
  const mediaSource=window.ManagedMediaSource||window.MediaSource;
  if(!(mediaSource && mediaSource.isTypeSupported)) return false;
  const v=MSE_VIDEO[mseCodecKey(srcCodec, bitDepth)];
  if(!v) return false;                       // unknown codec: do not gamble
  const a=MSE_AUDIO[String(audioCodec||"").toLowerCase()];
  if(!a) return false;
  return v.some(c=>{ try{ return mediaSource.isTypeSupported(`video/mp4; codecs="${c},${a}"`); }catch(e){ return false; } });
}
// Should this remux take the HLS path instead of progressive /stream.mp4?
//
// The server marks every probed remux because average bitrate cannot predict a
// transient path gap. This function is the browser's veto: only it knows
// whether hls.js and its MediaSource implementation accept the exact codec
// pair. A veto keeps progressive playback — stuttery is preferable to black.
function copyHlsMseOk(decision, video){
  if(!decision) return false;
  if(!(window.Hls && Hls.isSupported())) return false;
  if(useNativeHls(video)) return false;      // Safari already routes here below
  const src=decision.source||{};
  // What the audio will actually be on the wire: the server re-encodes to AAC
  // when the browser can't take the source, so ask about what arrives.
  const track=(decision.audio||[]).find(a=>a.default) || (decision.audio||[])[0];
  const audio=decision.transcode_audio? "aac" : (track&&track.codec);
  return mseCanTake(src.video_codec, audio, src.bit_depth);
}
function segmentedRemuxOk(decision, video){
  if(!decision || !decision.prefer_segmented) return false;
  if(noSegments()) return false;             // the viewer asked for one stream
  return copyHlsMseOk(decision,video);
}
// Catch the hitch itself, frame by frame.
//
// Every aggregate on the stats panel has now come back healthy on a session
// that visibly stutters: full buffer, hardware decode, 4 dropped frames in
// 699, no quota refusals. That is not a contradiction — it means the artifact
// is smaller than any of those measurements. `requestVideoFrameCallback` is
// the only API that sees individual presented frames, so it is the only place
// left to look.
//
// Five distinct faults, kept apart because they have different causes:
//   back    the media clock moved BACKWARDS between presented frames. This is
//           the "it repeats a frame" report, and it is what a resync looks
//           like from the outside.
//   held    the same frame presented again: the media clock did not advance a
//           whole frame while the screen refreshed. Nothing was lost, but the
//           picture stopped for a beat — the other half of "backs up a frame
//           and then resumes", and invisible to every counter we have.
//   late    the fault this callback cannot see directly, and the one a viewer
//           actually watches: the compositor HELD the previous frame for an
//           extra refresh or two. No new frame is presented during the hold,
//           so no callback fires — the evidence is the NEXT frame arriving
//           with a normal media step but a display-clock step nearly twice as
//           long. Judged on expectedDisplayTime, the compositor's own
//           schedule, not the callback timestamp, which runs on the main
//           thread and jitters with it.
//   gap     media time advanced past frames that were never presented at all,
//           which `droppedVideoFrames` does not count because they were never
//           decoded.
//   slow    this frame took far longer through the decoder than this session's
//           own typical frame — an OUTLIER, not an absolute duration.
//
// `slow` was originally "processingDuration over 40ms", and that was wrong on
// its face. processingDuration is measured from submitting the encoded packet
// to the frame being ready, so on a pipelined hardware decoder fed from a 12s
// buffer it is mostly queue latency: packets are submitted long before they
// are needed and sit there. Against a 41.7ms frame budget it reported 993 slow
// frames out of 1249 on a session whose decoder was keeping up perfectly —
// 80%, which is not a signal, it is a broken instrument, and it buried the one
// row on the panel that had actually found something.
//
// So the question becomes relative: is THIS frame unusual for this session?
// The median is the session's own pipeline latency, whatever that happens to
// be, and an outlier against it is a real stumble. It must also miss its frame
// budget, because 3x a 2ms median is still 6ms and nobody sees that.
const HITCH_GAP_FRAMES=2.5;
const HITCH_SLOW_FACTOR=3;
// Frames of history before any judgement. The nominal interval has to be
// measured, not assumed: the source rate is anywhere from 23.976 to 60.
const HITCH_WARMUP=12;
const HITCH_WINDOW=120;
// How close in time a player event has to be to count as the cause. Segments
// are 1.75s apart, so a 150ms window puts the odds of an innocent coincidence
// under one in ten — which is what makes "12 of 13 hitches landed on a flush"
// mean something.
const HITCH_NEAR_MS=150;
// A late frame has to be this far behind the compositor's schedule to count.
// The floor clears 3:2 pulldown on a 60 Hz panel, whose display intervals
// legitimately alternate 33/50 ms around a 42 ms mean (±8 ms of jitter is
// healthy); three-quarters of a frame catches a whole extra refresh at any
// display rate from 24 Hz up.
const HITCH_LATE_FLOOR_MS=25;
const HITCH_LATE_FRACTION=0.75;
function markEvent(kind){
  const p=PLAYER; if(!p) return;
  (p.marks||(p.marks={}))[kind]=performance.now();
}
// Segment boundaries from the playlist itself, for transports that have no
// hls.js to announce them. Safari's native HLS session showed 41 drops at
// segment cadence with NO attribution line — blame() had nothing to match
// against, because FRAG_CHANGED is an hls.js event. The playlist knows where
// every boundary is, and cumulative EXTINF is the same timeline the
// element's mediaTime runs on (both start at the session's own zero).
function parseSegTimes(text){
  const out=[]; let t=0;
  for(const line of String(text).split("\n")){
    const m=/^#EXTINF:([\d.]+)/.exec(line.trim());
    if(m){ t+=parseFloat(m[1]); out.push(+t.toFixed(3)); }
  }
  return out;
}
async function refreshSegTimes(){
  const p=PLAYER;
  if(!playbackOwnsAttachedMedia(p) || !p.segSrc || p.hls) return;
  const source=p.segSrc, attachment=p.mediaAttachment;
  const current=()=>playbackOwnsAttachedMedia(p)&&p.segSrc===source&&p.mediaAttachment===attachment;
  const now=performance.now();
  if(p._segFetch && now-p._segFetch<9000) return;   // the playlist grows ~every 4s at 2x
  p._segFetch=now;
  try{
    const r=await fetch(source,{cache:"no-store"}); if(!r.ok||!current()) return;
    const times=parseSegTimes(await r.text());
    if(current() && times.length>((p.segTimes&&p.segTimes.length)||0)) p.segTimes=times;
  }catch(e){}
}
// A frame callback registered before a new media execution belongs to the
// predecessor, even if the browser delivers it after the source was replaced.
function queuePlaybackFrame(v,p,callback){
  const epoch=p.controlPresentationEpoch||0;
  // One subscription belongs to this player and element. Retire it when an
  // element is adopted or reused, including callbacks already queued by the
  // browser before cancellation.
  if(p.controlFrameCancel) p.controlFrameCancel();
  if(v._plurxFrameCancel) v._plurxFrameCancel();
  let id=null, generation=0, retired=false;
  const cancel=()=>{
    generation++;
    if(id!=null&&typeof v.cancelVideoFrameCallback==="function") v.cancelVideoFrameCallback(id);
    id=null;
  };
  const clear=()=>{
    retired=true;cancel();
    v.removeEventListener("emptied",suspend);
    v.removeEventListener("loadeddata",ready);
    v.removeEventListener("canplay",ready);
    v.removeEventListener("seeked",ready);
    if(p.controlFrameCancel===clear) p.controlFrameCancel=null;
    if(v._plurxFrameCancel===clear) v._plurxFrameCancel=null;
  };
  const suspend=()=>{if(PLAYER!==p) clear();else cancel();};
  const ready=()=>{
    if(retired) return;
    if(PLAYER!==p){clear();return;}
    // Safari native HLS can strand a request registered before the initial
    // load/seek. Subscribe against current data after the seek completes,
    // and renew it when a source load replaces it. A request already armed
    // with current data survives ordinary seeks so it can observe the landing
    // frame, including when playback stays paused.
    // Readiness only permits registration; actual callbacks remain the proof
    // of presentation used by the existing progress detector.
    if(id!=null||v.readyState<2||v.seeking) return;
    const registration=++generation;
    id=v.requestVideoFrameCallback((now,meta)=>{
      if(registration!==generation) return;
      id=null;clear();
      if(PLAYER===p) callback(now,meta,epoch);
    });
  };
  p.controlFrameCancel=clear;v._plurxFrameCancel=clear;
  v.addEventListener("emptied",suspend);
  v.addEventListener("loadeddata",ready);
  // loadeddata is once per resource; canplay also covers buffer replenishment.
  v.addEventListener("canplay",ready);
  v.addEventListener("seeked",ready);
  ready();
}
function armHitchDetector(v){
  const p=PLAYER; if(!p) return;
  p.controlHasFrameCallbacks=!!v.requestVideoFrameCallback;
  if(!p.controlHasFrameCallbacks) return;
  p.hitches={back:0, held:0, late:0, gap:0, drop:0, slow:0, worst:0, last:null, at:[], fps:null,
             n:0, near:{}, flushEdge:null, skewMax:0, skewAt:null, decodeMs:null, frames:0,
             rate:null, renderedFps:null};
  // When the detector armed, so "zero faults" can be told apart from "zero
  // callbacks". Safari has shipped rVFC for years and still declines to fire
  // it on some pipelines — a session that stutters visibly while this
  // instrument records nothing is the instrument's fault to disclose.
  p.hitchArmedAt=performance.now();
  let prev=null;
  // The nominal frame interval, from HISTORY — never from the step under
  // test. Deriving it from the current step lets a skip define itself as
  // normal (dt over one presented frame IS the frame interval, so nothing can
  // ever exceed it), which is how the first cut of this scored a five-frame
  // jump as healthy. A median, because one outlier must not move it.
  const seen=[];
  const median=(a)=>{ const s=a.slice().sort((x,y)=>x-y); return s[s.length>>1]; };
  const nominal=()=>seen.length<HITCH_WARMUP?null:median(seen);
  // The same treatment for decode cost. This session's own median IS the
  // baseline; the absolute number is a property of the decoder's pipeline
  // depth, not of whether it is coping.
  const decodes=[];
  const typicalDecode=()=>decodes.length<HITCH_WARMUP?null:median(decodes);
  // The REALIZED playback rate: media time advanced per wall-clock second,
  // over a rolling window. `video.playbackRate` says what was asked for;
  // this says what is happening — and the gap between them is how Safari's
  // live-edge chasing shows itself. A native HLS session on our growing
  // EVENT playlist (no ENDLIST until the film is fully written) can be
  // rate-matched toward the live edge by the browser without ever admitting
  // it in playbackRate, and 5-10% fast is judder a viewer sees and every
  // per-frame counter forgives. Window ~10s so main-thread jitter in the
  // callback timestamps averages out; cleared on pause/seek because a ring
  // spanning a seek measures the seek.
  const rateWin=[];
  const noteRate=(now, mediaTime, pf, h)=>{
    // A ring that spans a timeline discontinuity measures the discontinuity.
    // Pause and seek are cleared by the caller, but a SESSION REPLACEMENT is
    // not: teardownHls()+attachHls() for a quality switch or a stall reopen
    // rebases mediaTime and resets presentedFrames without ever setting
    // v.paused or v.seeking. Sampling across that produced the nonsense in
    // production — `rate=-53.307 of 1 fps=-91.5`, logged five seconds after a
    // rung change — and those values feed the chase beacon and the stats
    // panel. Any backwards step in either counter starts a new window.
    const last=rateWin[rateWin.length-1];
    if(last && (mediaTime<last.m || pf<last.p)){
      rateWin.length=0;
      h.rate=null; h.renderedFps=null;
    }
    rateWin.push({w:now, m:mediaTime, p:pf});
    while(rateWin.length>240) rateWin.shift();
    const a=rateWin[0], b=rateWin[rateWin.length-1];
    const span=b.w-a.w;
    if(span>4000){
      const played=b.m-a.m, shown=b.p-a.p;
      // Belt and braces: only publish figures that are physically possible.
      // A rate or an fps that is negative, or not finite, is a measurement
      // artefact and must not reach a beacon that reads it as evidence.
      h.rate = played>=0 && Number.isFinite(played)
        ? +((played*1000)/span).toFixed(3)
        : null;
      // Frames that actually reached the screen, per second — the honest
      // "rendered" figure. Same window as the rate so they agree about time.
      h.renderedFps = shown>=0 && Number.isFinite(shown)
        ? +((shown*1000)/span).toFixed(1)
        : null;
    }
  };
  const step=(now, meta, epoch)=>{
    // Bail the moment this playback is replaced, or the callback outlives its
    // PLAYER and starts recording another stream's frames as this one's.
    if(PLAYER!==p) return;
    if(!playbackOwnsAttachedMedia(p)){
      prev=null;rateWin.length=0;
      queuePlaybackFrame(v,p,step);return;
    }
    p.controlPresentedFrames=(p.controlPresentedFrames||0)+1;
    if(epoch===(p.controlPresentationEpoch||0))
      settlePlaybackControlSeek(v,p,meta.mediaTime,p.controlPresentedFrames);
    p.hitches.frames++;
    if(v.paused || v.seeking){
      // Clear the MEASUREMENT with the window: a rate left standing after a
      // seek is last session's number wearing this one's label, and the
      // chase beacon below must never fire on it.
      rateWin.length=0;
      p.hitches.rate=null; p.hitches.renderedFps=null;
    } else {
      noteRate(now, meta.mediaTime, meta.presentedFrames||0, p.hitches);
      reportRateChase(v);
    }
    // Native-transport boundary marks: crossing a playlist boundary is this
    // path's FRAG_CHANGED. Checked before the fault branch so a fault AT the
    // boundary sees the mark in the same callback. A jump re-derives the
    // index instead of marking every boundary the seek flew over.
    const st=p.segTimes;
    if(st && st.length && !v.seeking){
      if(p._segIdx==null || p._segIdx>st.length
         || (p._segIdx>0 && meta.mediaTime<st[p._segIdx-1]-2)
         || (p._segIdx<st.length && meta.mediaTime>st[p._segIdx]+4)){
        const i=st.findIndex(t=>t>meta.mediaTime);
        p._segIdx=(i<0)?st.length:i;
      }
      if(p._segIdx<st.length && meta.mediaTime>=st[p._segIdx]){
        // Stamped with the callback's own timestamp, not performance.now():
        // they are the same clock in a browser, and blame() compares against
        // callback time — one clock for both sides, no skew to argue about.
        (p.marks||(p.marks={})).frag=now; p._segIdx++;
      }
    }
    if(prev!=null && !v.paused && !v.seeking){
      const h=p.hitches;
      const dt=meta.mediaTime-prev.mediaTime;
      const dn=Math.max(1, meta.presentedFrames-prev.presentedFrames);
      const nom=nominal();
      if(nom) h.fps=1/nom;
      // How far the frame ON SCREEN is from the element's own playback
      // position. With an audio track that position IS the audio clock, so
      // this is the A/V skew — and a renderer repeating video frames to wait
      // for audio is the one explanation for a periodic hitch that leaves
      // every buffer, decode and delivery counter healthy. Healthy playback
      // sits within about a frame; a sync correction swings it and puts the
      // swing next to the hitch that it caused.
      const ct=(typeof v.currentTime==="number" && v.currentTime>0)?v.currentTime:null;
      const skew=ct==null?null:meta.mediaTime-ct;
      if(skew!=null && nom && Math.abs(skew)>Math.abs(h.skewMax)) h.skewMax=+skew.toFixed(3);
      // What the player was doing when this frame went wrong. Called on every
      // fault, before the counters, so the attribution covers all of them.
      const blame=()=>{
        h.n++;
        PLAYBACK_LIFETIME_HITCHES++;
        if(skew!=null) h.skewAt=+skew.toFixed(3);
        const m=p.marks||{};
        for(const k of ["flush","frag","append"]){
          const t=m[k];
          if(t!=null && now-t>=0 && now-t<HITCH_NEAR_MS){
            h.near[k]=(h.near[k]||0)+1;
            if(k==="flush" && m.flushEdge!=null) h.flushEdge=m.flushEdge;
          }
        }
      };
      // This frame's decode cost, said alongside every fault. The one report
      // that can separate "the decoder spiked on the giant IRAP" from every
      // buffer- and container-shaped theory is the decode time AT the hitch
      // against the session's own typical — so it goes in the words, where a
      // screenshot carries it.
      const pd=meta.processingDuration;
      const fx=()=>{
        const typ=typicalDecode();
        return (pd!=null && typ!=null)
          ? ` · decode ${(pd*1000).toFixed(0)}ms (typ ${(typ*1000).toFixed(0)})` : '';
      };
      if(dt<-0.001){
        blame();
        h.back++; h.last=`stepped back ${(-dt*1000).toFixed(0)}ms at ${meta.mediaTime.toFixed(1)}s${fx()}`;
        h.at.push(+meta.mediaTime.toFixed(2));
      } else if(nom && dt<nom*0.5){
        blame();
        h.held++; h.last=`held a frame at ${meta.mediaTime.toFixed(1)}s${fx()}`;
        h.at.push(+meta.mediaTime.toFixed(2));
      } else if(nom && dt/nom-dn>HITCH_GAP_FRAMES){
        blame();
        // Frames the stream moved past that never reached the screen. Counted
        // against `dn` on purpose: the callback can coalesce and report three
        // frames at once, and three frames PRESENTED in one callback is the
        // browser batching, not a stutter — the viewer saw all of them.
        h.gap++; h.worst=Math.max(h.worst, (dt-dn*nom)*1000);
        h.last=`skipped ${((dt-dn*nom)*1000).toFixed(0)}ms at ${meta.mediaTime.toFixed(1)}s${fx()}`;
        h.at.push(+meta.mediaTime.toFixed(2));
      } else if(nom && dn===1 && dt>nom*1.6 && dt<nom*2.6){
        // Exactly one frame missing: one presented callback, media two frames
        // on. The gap threshold above deliberately tolerates this — Chrome's
        // coalescing makes small dt/dn mismatches routine — which made the
        // single dropped frame, Safari's signature fault at one every few
        // seconds and plainly visible to a viewer, the one artifact this
        // instrument refused to count (17 in droppedVideoFrames, zero on this
        // row). dn===1 keeps the coalescing exemption: several frames
        // PRESENTED in one callback is batching, not a drop.
        blame();
        h.drop=(h.drop||0)+1;
        h.last=`dropped a frame at ${meta.mediaTime.toFixed(1)}s${fx()}`;
        h.at.push(+meta.mediaTime.toFixed(2));
      } else {
        // The media step is normal — but was it ON TIME? A compositor hold
        // never presents a frame, so it never fires this callback; the next
        // frame lands here with dt ≈ one frame and a display-clock step of
        // two. expectedDisplayTime is compared against the media step scaled
        // by the playback rate; the pulldown-safe threshold is above.
        const eDt=(meta.expectedDisplayTime!=null && prev.expectedDisplayTime!=null)
          ? meta.expectedDisplayTime-prev.expectedDisplayTime : null;
        const rate=v.playbackRate||1;
        const lateMs=(eDt!=null && dt>0)? eDt-(dt*1000)/rate : null;
        if(nom && lateMs!=null && dt<nom*2.5
           && lateMs>Math.max(HITCH_LATE_FLOOR_MS, nom*1000*HITCH_LATE_FRACTION)){
          blame();
          h.late++; h.worst=Math.max(h.worst, lateMs);
          h.last=`${lateMs.toFixed(0)}ms late at ${meta.mediaTime.toFixed(1)}s${fx()}`;
          h.at.push(+meta.mediaTime.toFixed(2));
        }
        if(dt>0) seen.push(dt/dn);
        if(seen.length>HITCH_WINDOW) seen.shift();
        // Judged against this session's own median, and only once there is a
        // median to judge against. A frame also has to miss its frame budget
        // to count: three times a 2ms baseline is 6ms, which nobody sees.
        if(pd!=null){
          const typ=typicalDecode();
          if(typ!=null){
            h.decodeMs=+(typ*1000).toFixed(1);
            if(pd>typ*HITCH_SLOW_FACTOR && (!nom || pd>nom)) h.slow++;
            else { decodes.push(pd); if(decodes.length>HITCH_WINDOW) decodes.shift(); }
          } else {
            decodes.push(pd);
          }
        }
      }
      if(h.at.length>40) h.at.shift();
    }
    prev=meta;
    queuePlaybackFrame(v,p,step);
  };
  queuePlaybackFrame(v,p,step);
}
// The spacing between hitches, which is the thing that identifies the cause.
// Segments are ~1.75s here and the report is "every 5-10 seconds"; a median
// interval says which of those it actually tracks, and neither a count nor a
// rate can.
function hitchInterval(){
  const at=(PLAYER&&PLAYER.hitches&&PLAYER.hitches.at)||[];
  // Five events before claiming a cadence: three lates clustered at startup
  // produced "every ~0.0s", which reads as an emergency and means nothing.
  if(at.length<5) return null;
  const d=[]; for(let i=1;i<at.length;i++){ if(at[i]>at[i-1]) d.push(at[i]-at[i-1]); }
  if(!d.length) return null;
  d.sort((a,b)=>a-b);
  return d[Math.floor(d.length/2)];
}
// The attribution, in words, and only once there is enough of it to mean
// something. Segments arrive every 1.75s, so a 150ms window catches roughly
// one hitch in ten by luck alone — a share well above that is a finding, and
// anything at or below it is this line saying so by not appearing.
function hitchBlame(){
  const h=(PLAYER&&PLAYER.hitches)||null;
  if(!h||!h.n||h.n<5) return "";
  const bits=[];
  for(const k of ["flush","frag","append"]){
    const n=h.near[k]||0;
    if(n*4>=h.n) bits.push(`${n}/${h.n} at a ${k==="frag"?"segment boundary":k}`);
  }
  if(h.near.flush && h.flushEdge!=null) bits.push(`eviction ended ${h.flushEdge}s behind the picture`);
  // Only worth a line once it is bigger than a frame — below that it is the
  // ordinary distance between the frame on screen and the reported position.
  if(h.fps && Math.abs(h.skewMax)>1.5/h.fps)
    bits.push(`clock skew up to ${(h.skewMax*1000).toFixed(0)}ms`+
      (h.skewAt!=null?`, ${(h.skewAt*1000).toFixed(0)}ms at the last one`:''));
  // Stated as context, never as a fault. A deep decode pipeline is normal and
  // says nothing about whether it is keeping up — which is the mistake the
  // `slow` counter used to make out loud.
  if(h.decodeMs!=null) bits.push(`decode ${h.decodeMs}ms typical`);
  return bits.join(" · ");
}
