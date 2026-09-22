"use strict";
// ---- decode margin ---------------------------------------------------------
// The stutter investigation's verdict (docs/streaming/STUTTER-4K.md §5.3): on the 4K
// HEVC remux the client's TYPICAL decode was 41.6ms against a 41.7ms frame
// budget — the decoder running at exactly realtime, so every spike surfaced
// as a held frame — while the same machine decodes the 1080p transcode at
// 6ms and absorbs hundreds of spikes silently. No server change makes a
// client's decoder faster; the honest response is to measure the margin and
// route around it. The thresholds: a median at 80% of the frame budget
// leaves too little room for the giant-IRAP spikes that actually occur.
// Clearing does NOT use a decode number at all — see `maybeDecodeRescue`: it
// waits for the faults to be absent instead, because the same metric read the
// other way round measures pipeline depth and gets BIGGER as playback gets
// healthier.
// **The decode figure is not allowed to decide this any more.**
//
// The trigger used to require `decodeMs >= 80%` of the frame budget, on the
// reading that a decoder pinned to the budget has no room for a spike. The
// clearing condition was fixed a day earlier for reading the same number
// backwards; the trigger was left alone, which was inconsistent and wrong in
// the same way.
//
// What settled it was one screenshot. A 1920x1080 transcode at 7.5 Mb/s, zero
// dropped frames, hardware decode, reporting **83 ms/frame against a 42 ms
// budget** — the same "no slack" verdict as the 4K remux it had just rescued
// the viewer away from. A metric that condemns an 8 Mb/s 1080p stream on the
// machine it is measuring has no discriminating power at all: it is
// `processingDuration`, which is queue latency on a pipelined hardware decoder
// and gets LARGER the healthier the pipeline is. With that gate always true,
// the rescue was in practice firing on four hitch events in twenty seconds —
// which any two-hour film will produce for a dozen transient reasons.
//
// So judge the thing the viewer actually loses: frames that were never
// presented, or presented out of order. Not `late` (the compositor holding a
// frame an extra refresh — real, but it happens on the transcode too, so it
// cannot tell the two apart) and not `held` (a repeat, with nothing lost) and
// emphatically not `slow`, which is an outlier count against the session's own
// median and says nothing about capacity.
//
// And judge it as a RATE over a long window. The old 20-second window could
// not tell a bad scene from a bad machine.
const MARGIN_LOST_PER_MIN=6;
const MARGIN_MIN_SECS=150;
const MARGIN_MIN_LOST=15;
const MARGIN_CLEAR_SECS=60;
// Frames lost per minute of PLAYED video — the one measure both the rescue and
// its undo are judged on, so that "it got better" is literally the inverse of
// "it went bad" rather than a second heuristic that can drift away from the
// first. Returns null when there is not enough playback to say anything.
function lostFrameRate(h, playedSecs){
  return PlaybackPolicy.lostFrameRate(h, playedSecs);
}
// The verdict, pure so it is testable: null, or the numbers that justify a
// rescue. The absolute floor sits beside the rate so that a single ugly
// stretch early on cannot condemn the session on arithmetic alone.
function decodeMarginVerdict(h, playedSecs){
  return PlaybackPolicy.decodeMarginVerdict(h, playedSecs, {
    lostPerMinute:MARGIN_LOST_PER_MIN,
    minimumSeconds:MARGIN_MIN_SECS,
    minimumLost:MARGIN_MIN_LOST
  });
}
// What this DEVICE could not present smoothly, remembered against a versioned
// media-load identity: codec/profile, resolution, bit depth, dynamic range,
// rich HDR shape, and a 10 Mb/s bitrate bucket. One demanding DV encode must
// never condemn every 4K HEVC title. Explicitly chosen Original always wins —
// the limit only steers Auto — and a clean remeasurement clears only this
// exact identity, so a sibling stream keeps its own evidence.
// How long a measured limit is believed.
//
// It is evidence about one device, one browser build and one stream at one
// moment — and every part of that moves. Browsers update, drivers change, and
// the SERVER's output changes underneath it: this whole mechanism exists
// because a 4K remux stuttered, and it stopped stuttering without the device
// changing at all. An entry with no expiry is not a measurement any more, it
// is a belief. Thirty days is long enough that a viewer who watches the same
// film every week never re-measures, and short enough that nobody is stuck
// with a verdict from a machine they have since replaced.
const DECODE_LIMIT_TTL_MS=PlaybackPolicy.DECODE_LIMIT_DEFAULTS.ttlMs;
// How long before Auto re-tests a limit by itself.
//
// The limit is only ever re-measured on an explicit Quality → Original, which
// assumes the viewer knows the mechanism exists, knows their picture is soft
// because of it, and knows which menu undoes it. Most people know none of
// those things, so in practice the verdict was permanent — a device that grew
// out of it (a browser update, a GPU driver, or the server fixing the thing
// that made the stream stutter in the first place, which is exactly what
// happened here) stayed on a transcode forever.
//
// So every week, one Auto session ignores the entry and plays the source. If
// it goes well, the entry clears itself and 4K comes back with nobody having
// touched anything. If it stutters, the rescue fires as it always did and
// stamps a fresh date, pushing the next re-test out another week — so the
// worst case is ~20 seconds of hitches once a week, against the alternative
// of never getting the source back at all.
const DECODE_LIMIT_RETEST_MS=PlaybackPolicy.DECODE_LIMIT_DEFAULTS.retestMs;
function decodeLimitKey(src){ return PlaybackPolicy.decodeLimitIdentity(src); }
function decodeLimitLabel(src){ return PlaybackPolicy.mediaLoadLabel(src); }
// Reads and prunes. Anything past its shelf life is dropped on the way out,
// so an expired verdict cannot steer a decision even once — and so is anything
// written before the trigger stopped believing the decode figure. Those
// entries carry `decode_ms` and no `rate`, and they were produced by a gate
// that reads true on every stream this browser has ever played, including an
// 8 Mb/s 1080p transcode with no dropped frames. They are not measurements of
// anything. Dropping them re-measures whatever was real, and spares the
// viewer having to find a button to undo a verdict they were never told about.
function decodeLimits(){
  let raw;
  try{ raw=JSON.parse(localStorage.getItem("plurx_decode_limits")||"{}")||{}; }
  catch(e){ try{ localStorage.removeItem("plurx_decode_limits"); }catch(_){} return {}; }
  const result=PlaybackPolicy.pruneDecodeLimits(raw,{nowMs:Date.now(),ttlMs:DECODE_LIMIT_TTL_MS});
  if(result.changed){
    try{ localStorage.setItem("plurx_decode_limits",JSON.stringify(result.limits)); }catch(e){}
  }
  return result.limits;
}
// Forget everything measured on this browser.
//
// The escape hatch, and it exists because the automatic ones cannot be trusted
// to cover every case: this state is invisible, it silently steers Auto, and
// its self-clearing condition was unreachable for a month before anyone
// noticed (docs/PLAYBACK.md). A viewer whose player is quietly transcoding a
// film it could direct-play needs a way to say "measure it again" that does
// not involve a developer console.
function forgetDecodeLimits(){
  let n=0;
  try{ n=Object.keys(decodeLimits()).length; localStorage.removeItem("plurx_decode_limits"); }catch(e){}
  toast(n? `Forgot ${n} measured playback limit${n===1?"":"s"} — Auto will re-measure`
         : "No measured playback limits to forget");
  return n;
}
// Forget the one that applies to what is playing now, and act on it: an Auto
// session steered by it is restarted so the viewer sees the effect rather than
// being told about it.
function forgetDecodeLimitHere(){
  const p=PLAYER; if(!p) return;
  const had=clearDecodeLimit(p.source||{});
  closeMenu();
  if(!had){ toast("Nothing measured for this stream"); return; }
  toast("Forgotten — measuring this stream again");
  if(qualityForce()==='auto' && p.fileId){
    PENDING_ATTEMPT_REASON="decode-limit-cleared";
    const v=document.getElementById("video");
    const pos=positionForPlaybackIntent(v,p);
    beginPlaybackControlSeek(p,pos);
    play(p.fileId, p.title||"", Math.round(pos*1000), p.knownDur||0, p.meta);
  }
}
function decodeLimitFor(src){ const k=decodeLimitKey(src); return k? (decodeLimits()[k]||null) : null; }
function rememberDecodeLimit(src, v){
  const k=decodeLimitKey(src); if(!k||!v) return;
  try{ const l=decodeLimits();
       l[k]={lost:v.lost, rate:v.rate, secs:v.secs,
             decode_ms:v.decodeMs, budget_ms:v.budgetMs,
             label:decodeLimitLabel(src), at:Date.now()};
       localStorage.setItem("plurx_decode_limits", JSON.stringify(l)); }catch(e){}
}
// What an entry says it measured. `decodeLimits()` drops anything without a
// rate, so this only ever describes the current shape.
function decodeLimitWhy(lim){
  return lim&&lim.rate!=null ? `lost ${lim.lost} frames in ${lim.secs}s (${lim.rate}/min)` : "";
}
function clearDecodeLimit(src){
  try{ const result=PlaybackPolicy.withoutLearnedDecodeLimit(decodeLimits(),src);
       if(!result.removed) return false;
       localStorage.setItem("plurx_decode_limits",JSON.stringify(result.limits)); return true;
  }catch(e){ return false; }
}
// What Settings shows about the limits this browser is carrying: the entries
// themselves, not just a button, because "forget the thing you cannot see" is
// not an offer anybody can evaluate.
function decodeLimitsSummary(){
  const l=decodeLimits(), keys=Object.keys(l);
  if(!keys.length) return `<span class="muted" style="font-size:13px">Nothing measured on this browser.</span>`;
  const items=keys.map(k=>`<span class="pill">${esc(l[k].label||k)} · ${l[k].rate} lost/min · ${
    esc(agoLabel(Math.round(l[k].at/1000)))}</span>`).join(" ");
  return `${items} <button class="ghost sm" onclick="forgetDecodeLimitsUI()">Forget ${keys.length===1?"it":"them all"}</button>`;
}
function forgetDecodeLimitsUI(){
  forgetDecodeLimits();
  const row=document.getElementById("dlrow");
  if(row) row.innerHTML=decodeLimitsSummary();
}
// Seconds of video this element has actually PLAYED — pauses and stalls do
// not count, which is what makes it the right observation clock for a
// decode-margin judgement.
function playedSecs(v){
  let s=0; try{ for(let i=0;i<v.played.length;i++) s+=v.played.end(i)-v.played.start(i); }catch(e){}
  return s;
}
// Buffer targets in SECONDS, bounded by BYTES — and the bytes are the point.
//
// §4.3 picked 60s forward and 30s back, correctly, for a TRANSCODE: its output
// is bounded by the rung (§4.6), so 90 seconds of an 8 Mb/s stream is ~90 MB
// and sits comfortably inside any browser's MSE quota.
//
// §4.3bis then started routing big remuxes down the same hls.js path, and a
// remux carries the SOURCE's bitrate. The same 90 seconds of a 69 Mb/s film is
// ~775 MB against a quota nearer 150 MB, so hls.js spends the whole playback
// appending, hitting QuotaExceededError, evicting, and appending again. The
// eviction runs near the playhead, and what a viewer sees is a hitch every few
// seconds that never lasts long enough to count as a stall.
//
// So: keep the seconds for anything whose bitrate is already bounded, and for
// a copied stream derive the seconds from a byte budget instead. The floor
// exists because a very large file would otherwise buffer almost nothing.
// What the browser will hold before it starts refusing appends. Chrome's video
// SourceBuffer quota is ~150 MB on a desktop and no hls.js setting raises it;
// this is that number with a little left over, because being wrong about it in
// the optimistic direction is what the whole comment below is about.
const MSE_QUOTA_BYTES=144e6;
// **An append is not free, and forgetting that cost a session.**
//
// The budget used to be resident-only: 96 MB forward, 32 MB back, capped at a
// 144 MB total — and it held, as long as the data arrived in small pieces. It
// arrived in 2-second pieces, so the browser was asked for at most another
// ~15 MB on top of what it already had, and 128 + 15 fitted.
//
// Then plurx took the cutting away from ffmpeg (see the segmenter) and the
// floor went to 6 s to stop losing a frame at every boundary. At 61 Mb/s a
// 6-second segment is 46 MB, and a segment that runs to the byte ceiling is
// 64 MB. The browser has to hold what it already has PLUS the whole segment
// being appended, so every append now asked for ~190 MB against a ~150 MB
// quota and was refused. hls.js responds by evicting, halving its own target
// and retrying; after `appendErrorMaxRetry` (3) failures on the same segment
// it gives up fatally. Reported from the couch on reference film F in Chrome, 2026-07-30:
// several rebuffers and then a freeze that would not resume. Safari never saw
// it, because native HLS does not use any of this.
//
// So the budget reserves room for the segment in flight:
//
//     forward + one segment + back  <=  MSE_QUOTA_BYTES
//
// The segment term is measured, not assumed: `EXT-X-TARGETDURATION` is the
// longest segment the session has actually published (copyseg keeps it
// honest), so `retuneBuffer` re-derives these the moment the first playlist
// lands. The constant below is only what we guess before that.
//
// A forward target smaller than one segment is not the deadlock it looks
// like: hls.js fetches whenever what is buffered ahead falls under the
// target, so an 8 s target with 9 s segments oscillates between 8 s and 17 s
// of runway. The peak is what the quota sees, which is why the peak is what
// this budgets.
const MSE_SEGMENT_CEILING_BYTES=64e6;   // copyseg's COPY_SEGMENT_MAX_BYTES
const MSE_SEGMENT_CEILING_SECS=15;      // copyseg's COPY_SEGMENT_MAX_SECS
// The back buffer holds video already watched — every byte of it is quota
// spent on something nobody will see again, and on this path a seek starts a
// fresh session anyway, so it buys a scrub of a second or two and nothing
// else. It gets a byte allowance rather than a seconds one, and the smallest
// floor that still permits a scrub.
const MSE_BACK_BYTES=12e6;
// Below this there is no budget left to divide and the stream does not belong
// on the copy path at all — a routing decision (§4.3bis) rather than one this
// function can make. It reports `fits:false` and takes what it can get.
const MSE_MIN_RESIDENT_BYTES=48e6;
// `segSecs` is the longest segment this session publishes, once known.
function bufferTargets(segSecs){
  const p=PLAYER||{};
  // Only a copied stream puts the source's own bitrate on the wire. A
  // transcode's output is bounded by the rung, and its SOURCE bitrate says
  // nothing about what it will send — using it here would shrink the buffer on
  // exactly the streams that do not need it.
  const bps=p.copyHls ? ((p.source&&p.source.bitrate)||0) : 0;
  if(!bps || bps<=0) return {fwd:60, back:30, budgeted:false, fits:true};
  const Bps=bps/8;
  // **Seconds times the average bitrate is not the size of a segment.**
  //
  // This budgeted `TARGETDURATION * bitrate` and got it badly wrong in the one
  // direction that matters. copyseg cuts on TWO ceilings — 64 MB or 15 s,
  // whichever comes first — so a 15-second segment exists precisely BECAUSE
  // that stretch of film was quiet enough for 15 s to fit under 64 MB. Costing
  // it at the 69 Mb/s average says 129 MB, which is not merely wrong, it is
  // impossible: the byte ceiling forbids it.
  //
  // What that did on reference film F in Chrome: seg 129 MB left 15 MB of resident budget,
  // the floor clamped it to 48 MB, and the client ran a **4 s forward buffer**
  // on a 69 Mb/s remux — 322 slow frames and a ten-second freeze, worse than
  // the bug this budget was written to fix a few hours earlier.
  //
  // So the byte ceiling is the cap, always. It is a server-side guarantee, not
  // an estimate, and `PLAYER.segBytes` replaces it with the largest segment
  // this session has actually appended as soon as one has (see FRAG_LOADED) —
  // measured beats derived, and both beat multiplying by an average.
  const guess=Math.min(MSE_SEGMENT_CEILING_SECS*Bps, MSE_SEGMENT_CEILING_BYTES);
  const seen=(p.segBytes|0)>0 ? Math.min(p.segBytes, MSE_SEGMENT_CEILING_BYTES) : 0;
  const segBytes=seen || Math.min(segSecs>0?segSecs*Bps:Infinity, guess);
  const seg=segBytes/Bps;
  // What is left for resident data once the append in flight is paid for.
  const resident=MSE_QUOTA_BYTES-segBytes;
  const fits=resident>=MSE_MIN_RESIDENT_BYTES;
  const room=Math.max(MSE_MIN_RESIDENT_BYTES, resident);
  const back=Math.max(1, Math.min(30, MSE_BACK_BYTES/Bps, room*0.25/Bps));
  // Forward is simply what remains. Deliberately no seconds floor: the segment
  // IS the runway, because hls.js refills the moment the buffer dips under the
  // target, so a 4 s target with 7 s segments still oscillates around 11 s
  // buffered. A floor here could only promise bytes the quota will refuse —
  // which is precisely the bug this rewrite exists to fix, and a floor is how
  // it would come back.
  const fwd=Math.max(1, Math.min(60, (room-back*Bps)/Bps));
  // Rounded DOWN, both of them. Rounding a target up spends bytes the budget
  // above did not allocate, which on a 4K remux is megabytes per second of
  // rounding error and is exactly the sort of small optimism the quota
  // punishes with a freeze.
  const rf=Math.max(1, Math.floor(fwd)), rb=Math.max(1, Math.floor(back));
  return {fwd:rf, back:rb, budgeted:true, fits,
          seg:+seg.toFixed(1), fwdBytes:Math.round(rf*Bps),
          peakBytes:Math.round((rf+seg+rb)*Bps)};
}
// Apply a budget to a live hls.js instance.
//
// `maxBufferSize` has to move with `maxBufferLength` or it silently wins:
// hls.js takes the LARGER of the seconds target and `8*maxBufferSize/bitrate`,
// so leaving the stock 60 MB there pins the forward buffer at 8 s on a 61 Mb/s
// stream no matter what the seconds say. That is the same class of mistake as
// the one above — a limit that reads like a cap and behaves like a floor.
function applyBufferTargets(hls, tgt){
  if(!hls||!hls.config||!tgt||!tgt.budgeted) return;
  hls.config.maxBufferLength=tgt.fwd;
  hls.config.backBufferLength=tgt.back;
  hls.config.maxBufferSize=tgt.fwdBytes;
}
// The playlist knows the real segment length; the constants above only
// guessed it. Runs once per session, on the first playlist that carries a
// target duration.
function retuneBuffer(segSecs, force){
  const p=PLAYER; if(!p||!p.hls||!(segSecs>0)) return;
  if(p.bufSegSecs===segSecs && !force) return;
  p.bufSegSecs=segSecs;
  const tgt=bufferTargets(segSecs);
  if(!tgt.budgeted) return;
  p.bufTarget=tgt;
  applyBufferTargets(p.hls, tgt);
  clientLog({level:"info",event:"buffer_budget",
    detail:`seg=${tgt.seg}s fwd=${tgt.fwd}s back=${tgt.back}s peak=${Math.round(tgt.peakBytes/1e6)}MB`,
    message:`buffer budget for a ${Math.round(((p.source&&p.source.bitrate)||0)/1e6)} Mb/s copy: `+
            `${tgt.fwd}s ahead + one ${tgt.seg}s segment + ${tgt.back}s behind = `+
            `${Math.round(tgt.peakBytes/1e6)}MB against a ${Math.round(MSE_QUOTA_BYTES/1e6)}MB quota`});
}
// Destroying an hls.js instance, in one place, because there are now two
// slots that hold one: the authoritative `PLAYER.hls` and a prepared
// successor. Both owe the same three things — keep the EWMA the instance paid
// to learn, destroy it so it stops polling a playlist forever, and clear the
// element's transport expectations.
function destroyHlsInstance(p,hls,element){
  if(!hls) return;
  const estimate=PlaybackPolicy.bandwidthSeedBps({
    outgoingEstimateBps:hls.bandwidthEstimate,
    priorKbps:p&&p.priorKbps
  });
  if(estimate&&p) p.bandwidthSeedBps=estimate;
  try{hls.destroy()}catch(e){}
  resetPlaybackTransportEvents(element);
}
function teardownHls(){
  if(PLAYER) cancelHlsStartup(PLAYER,'teardown');
  if(PLAYER) PLAYER.mediaAttachment=null;
  // A staged successor belongs to the stream it was prepared against. Any new
  // attach on the authoritative element — a seek, a quality change, a close —
  // is that stream ending, and §C7 requires the staging be settled rather than
  // left to the 330-second deadline: a preparation nobody answered holds this
  // session's only preparation slot for the rest of its life.
  abandonPreparedReplacement(PLAYER,"aborted","the playing stream was replaced");
  // The stream this change was asked against is ending. Whatever is replacing
  // it owns the viewer's intent now, and the ask is retired rather than
  // reopened a second time.
  supersedeDirectedChange(PLAYER);
  cancelPreparedFirstFrame(PLAYER);
  if(PLAYER&&PLAYER.hls){
    const video=document.getElementById("video");
    rememberPlaybackTransportIntent(video,PLAYER);
    PLAYER.internalMediaReset=true;
    pausePlaybackInternally(video);
    destroyHlsInstance(PLAYER,PLAYER.hls,video);
    PLAYER.hls=null;
  }
}
function resetMediaSource(v){
  if(PLAYER){
    cancelHlsStartup(PLAYER,'media_source_reset');
    PLAYER.mediaAttachment=null;
    rememberPlaybackTransportIntent(v,PLAYER);
    PLAYER.internalMediaReset=true;
  }
  try{ pausePlaybackInternally(v); v.removeAttribute("src"); v.load(); }
  catch(e){} finally { resetPlaybackTransportEvents(v); }
}
// load()/src changes discard queued media tasks. Expectations belong to that
// load and to the individual play promise, never to an unowned numeric count.
function resetPlaybackTransportEvents(v){
  if(v) v._plurxTransportEvents={play:[],pause:[]};
}
function playbackTransportEvents(v){
  return v._plurxTransportEvents||(v._plurxTransportEvents={play:[],pause:[]});
}
function setPlaybackMediaSource(v,url){
  v.src=url;
  resetPlaybackTransportEvents(v);
}
function retirePlaybackPredecessor(p){
  if(!p?.mediaPredecessor) return;
  const predecessor=p.mediaPredecessor;
  // Its generation stops being about anything the moment it is retired, and so
  // does every fault that was about it (contract §3.4).
  playbackSurfaceStep({retire:playbackSurfaceGeneration(predecessor)});
  PLAYER=predecessor;stopPlayerTimers();teardownHls();
  if(predecessor.sessionId)releaseSession(predecessor.sessionId);
  PLAYER=p;p.mediaPredecessor=null;
}
// Attachment ownership is not command ownership: a native seek or Pause can
// change the current command while this same decoder is still loading.
function beginPlaybackMediaAttachment(p){
  const token={id:(p._mediaAttachmentOrdinal||0)+1};
  p._mediaAttachmentOrdinal=token.id;
  p.mediaAttachment=token;
  // A deliberate reopen mints a new attachment. Clear the retired attempt
  // only after the old attachment can no longer pass its identity closure.
  p.terminalStop=null;
  // Where this attempt's generation takes the picture: every attach on every
  // route runs through here, and faults about any other generation are dropped.
  // That is what bounds a fault's life — identity, not a clock. (`STREAM_FAILURE`
  // keeps its own `FAILURE_FRESH_MS` freshness check, which decides whether a
  // refusal still has a sentence worth repeating; it is a different question.)
  playbackSurfaceStep({attach:playbackSurfaceGeneration(p)});
  return {current:()=>PLAYER===p&&p.mediaAttachment===token};
}
function applyPlaybackAttachmentPosition(v,p,attachment,startAt){
  if(!attachment.current()) return;
  const pending=p.controlSeek;
  const position=pending?Math.max(0,pending.targetMs/1000-(p.offset||0)):startAt;
  if(Number.isFinite(position)&&position>=0) try{v.currentTime=position;}catch(e){}
}
function pausePlaybackInternally(v){
  if(!v) return;
  // pause dispatches its event later, possibly after PLAYER was replaced.
  // Tag the actual transition on the persistent element, not the old player.
  if(!v.paused) playbackTransportEvents(v).pause.push({});
  v.pause();
}
function rememberPlaybackTransportIntent(v,p){
  if(p&&typeof p.wantsPlayback!=="boolean") p.wantsPlayback=!!v&&!v.paused&&!v.ended;
}
function applyPlaybackTransportIntent(v,p){
  if(!v||!p||PLAYER!==p) return;
  rememberPlaybackTransportIntent(v,p);
  if(p.wantsPlayback){
    const events=playbackTransportEvents(v), token={};
    if(v.paused) events.play.push(token);
    v.play().catch(()=>{
      const index=events.play.indexOf(token);
      if(index>=0) events.play.splice(index,1);
    });
  }else pausePlaybackInternally(v);
}
function handlePlaybackTransportEvent(v,p,event){
  if(!v||!p||PLAYER!==p) return;
  if(event==="pause"){
    if(playbackTransportEvents(v).pause.shift()) return;
    if(!v.paused) return; // queued native edge superseded by a newer Play
    if(v.ended||v.error) return;
    p.wantsPlayback=false;
    if(typeof play==='function'&&play.pendingIntent&&PLAY_OPEN_GATE.current(play.pendingIntent.attempt))
      play.pendingIntent.wantsPlayback=false;
    supersedePlaybackControlIntent(p);
    pauseHlsStartup(p);
    endWait(false);
    // §3.1: a `buffering` fault is about a player that WANTS media, and this
    // viewer does not any more. `endWait` clears the watchdog and the episode
    // bookkeeping and has never touched the surface, so without this the
    // spinner sat over a still frame until the generation changed — the overlay
    // outliving the thing it described, reintroduced by the migration itself.
    // The presenter is told the client's own fact; no detector, no timer.
    // Internal pauses never reach here (`playbackTransportEvents` filters them
    // above), which is right: this is the VIEWER's intent and nothing else.
    playbackSurfaceStep({playback_requested:false});
  }else if(event==="play"){
    if(playbackTransportEvents(v).play.shift()) return;
    if(v.paused) return; // queued native edge superseded by a newer Pause
    p.wantsPlayback=true;
    if(typeof play==='function'&&play.pendingIntent&&PLAY_OPEN_GATE.current(play.pendingIntent.attempt))
      play.pendingIntent.wantsPlayback=true;
    supersedePlaybackControlIntent(p);
    resumeHlsStartup(v,p);
    // Symmetry, and nothing more: resuming raises no fault back. If the buffer
    // is still empty the element fires `waiting` again and THAT is the raise.
    playbackSurfaceStep({playback_requested:true});
  }
}
// Every timer hanging off PLAYER, stopped. Anything that replaces or discards
// PLAYER must call this first: once the object is gone the handles are gone
// with it and the callbacks run forever.
function stopPlayerTimers(){
  if(!PLAYER) return;
  stopPlaybackControl(PLAYER);
  clearPendingSeekTimer();
  clearInterval(PLAYER.timer); PLAYER.timer=null;
  clearInterval(PLAYER.progressTimer); PLAYER.progressTimer=null;
  clearTimeout(PLAYER.idleTimer); PLAYER.idleTimer=null;
  clearTimeout(PLAYER.stallTimer); PLAYER.stallTimer=null;
  clearTimeout(PLAYER.waitTimer); PLAYER.waitTimer=null;
  PLAYER.samplingStopped=true;
}
// Every per-player sampling timer, armed together: the 500 ms progress tick —
// which is the stall detector, the presenter's only evidence and the clock its
// timed notices age against — and the 5 s controller tick.
//
// `play()` arms them when a stream attaches. `armStall` re-arms them for a
// stream that starts again after the recovery owner stopped the player, because
// §3.4's stop takes `stopPlayerTimers` with it: without this, Force transcode
// from a stalled prompt would resume into a player with no stall detection and
// a presenter with no evidence, and neither would come back until the next
// full open.
function armPlaybackSampling(v,p){
  if(!v||!p) return;
  clearInterval(p.progressTimer);
  p.progressTimer=setInterval(()=>{
    renderPlaybackSurface.waitDetail=playbackWaitLiveDetail(v,p);
    playbackProgressTick(v,p);
  },500);
  clearInterval(p.timer);
  p.timer=setInterval(()=>{
    if(!playbackOwnsAttachedMedia(p)) return;
    reportProgress(p.fileId); reportHitches(); maybeDecodeRescue();
    autoControllerTick().catch(()=>{}); refreshSegTimes(); libraryChannelTick();
  }, PlaybackPolicy.AUTO_DEFAULTS.sampleMs);
  p.samplingStopped=false;
}

