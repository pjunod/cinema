"use strict";

const assert = require("node:assert/strict");
const liveTv = require("../../crates/plurxd/src/web/live-tv.js");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const fs = require("node:fs");
const path = require("node:path");
const {shellSource} = require("./shell-source.js");
// The app's body rows, joined in served order.
// The page and the geometry it depends on: the Live TV screens are laid out
// by CSS that the assertions below read, so the stylesheet is part of "the
// shipped shell" here.
const shell = (() => { const s = shellSource(); return s.bodyScript + s.css; })();
function shipped(name) {
  const match = new RegExp(`\\n(?:async )?function ${name}\\(`).exec(shell);
  assert.ok(match, `missing shipped function ${name}`);
  const start = match.index + 1, end = shell.indexOf("\n}", start);
  assert.ok(end > start, `missing closing brace for ${name}`);
  const feedback=["stopLiveTv","watchLiveTv","liveTvAttachSession","resumeLiveTv","pauseLiveTv"].includes(name)
    ? shipped("liveTvPlaybackState")+"\n"+shipped("liveTvPlaybackFailure")+"\n" : "";
  return feedback+shell.slice(start, end + 2);
}

function test(name, run) {
  return Promise.resolve()
    .then(run)
    .then(() => process.stdout.write(`PASS ${name}\n`));
}

// Let real timers (the liveness probe) and the promises behind them finish.
function settled(ms) {
  return new Promise(done => setTimeout(done, ms)).then(() => Promise.resolve());
}

function deferred() {
  let resolve;
  const promise = new Promise((settle) => { resolve = settle; });
  return { promise, resolve };
}

function memoryStorage() {
  const values = new Map();
  return { get length() { return values.size; }, key: i => [...values.keys()][i],
    getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value),
    removeItem: key => values.delete(key) };
}

// Exercise the shipped lifecycle against the real lease queue. Deferred starts,
// play promises and fullscreen exits reproduce races without a tuner or clock waits.
function startupHarness(options={}) {
  const messages=[],starts=[],releases=[],polls=[];
  let clock=1000,plays=0,loads=0;
  const state={channels:["one","two"].map((id,i)=>({id,guide_number:`${i+1}.1`,guide_name:id,drm:false,support:"ready"})),serial:0};
  const video={paused:true,currentTime:0,textTracks:[],canPlayType:()=>"maybe",
    play(){plays++;return options.play?options.play(video):Promise.resolve();},
    pause(){video.paused=true;video.onpause?.();},
    removeAttribute(){video.src="";},load(){loads++;}};
  const nodes={"live-tv-video":video,"live-tv-host":{hidden:true,dataset:{mode:"slot"}},
    "live-tv-status":{hidden:true,dataset:{}},"live-tv-status-text":{},
    "live-tv-status-play":{},"live-tv-status-retry":{},
    "live-tv-status-offers":{children:[],replaceChildren(){this.children=[];},append(button){this.children.push(button);}}};
  const lease=new liveTv.Lease({
    start:async id=>{starts.push(id);return options.start?options.start(id):{session_id:`cap-${id}`};},
    release:async id=>{releases.push(id);},
    status:async()=>options.status?options.status():{state:"active"},keepalive:async()=>{},
  });
  const document={getElementById:id=>nodes[id]||null,visibilityState:"visible",querySelectorAll:()=>[],
    createElement:()=>({addEventListener(name,fn){this[name]=fn;}})};
  const functions=["liveTvPlaybackState","liveTvPlaybackFailure","detachLiveTvMedia","stopLiveTv",
    "watchLiveTv","liveTvAttachSession","resumeLiveTv","pauseLiveTv","liveTvNow","liveTvWatchChannel"];
  const controls=new Function("LIVE_TV","LIVE_TV_LEASE","document","PlurxLiveTv","performance","setInterval","clearInterval","options","messages",
    `const location={hash:'#/live-tv'},PAGE_RENDER_GENERATION=1,PLAYER=null,API='/api',window={};
     function liveTvMessage(message){messages.push(message);}
     function liveTvSelect(id){return watchLiveTv(LIVE_TV.channels.findIndex(channel=>channel.id===id));}
     function closeLiveTvStats(){} function liveTvCaptionTracks(){return [];}
     function liveTvRefreshCaptionControls(){} function liveTvInPip(){return false;}
     function liveTvTrackSlot(){}
     function liveTvShowHost(){document.getElementById('live-tv-host').hidden=false;}
     function liveTvSetMode(mode){document.getElementById('live-tv-host').dataset.mode=mode;}
     function exitLiveTvPresentation(){return options.exit?.();}
     ${functions.map(shipped).join('\n')}
     return {watchLiveTv,stopLiveTv,resumeLiveTv,pauseLiveTv,liveTvPlaybackFailure};`)(state,lease,document,liveTv,
    {now:()=>clock},fn=>{polls.push(fn);return polls.length;},()=>{},options,messages);
  return {...controls,state,lease,video,nodes,starts,releases,messages,polls,
    advance:ms=>{clock+=ms;},get plays(){return plays;},get loads(){return loads;}};
}

async function main() {
  await test("picture facts keep broadcast, planned frame and browser display distinct", () => {
    const delivery = {
      source: {width: 704, height: 480, sample_aspect_ratio: "40:33", video_codec: "mpeg2video"},
      output: {width: 704, height: 480, video_codec: "h264"},
      video_action: "encode",
      reasons: [{code: "new_code", explanation: "<img onerror=bad>"}],
    };
    const facts = liveTv.normalizeLiveTvPictureFacts({delivery,
      presentation: {width: 853, height: 480}, attachmentCurrent: true, nowSeconds: 1000});
    const formatted = liveTv.formatLiveTvPictureFacts(facts);
    assert.equal(formatted.source_resolution, "704×480");
    assert.equal(formatted.stream_frame, "704×480");
    assert.equal(formatted.stream_frame_note, "Planned output");
    assert.equal(formatted.decode_resolution, "853×480");
    assert.equal(formatted.source_display_aspect, "16:9");
    assert.equal(formatted.frame_comparison, "No resize planned");
    assert.equal(formatted.aspect_comparison, "Not verified");
    assert.equal(liveTv.liveTvReasonText(delivery.reasons), "<img onerror=bad>");
    assert.equal(liveTv.parsePictureRatio("0:1"), null);
    assert.equal(liveTv.parsePictureRatio("N/A"), null);
    const reduced = liveTv.formatLiveTvPictureFacts(liveTv.normalizeLiveTvPictureFacts({
      delivery: {...delivery, source: {...delivery.source, width: 1920, height: 1080, sample_aspect_ratio: "1:1"},
        output: {...delivery.output, width: 1280, height: 720}},
      presentation: {width: 1920, height: 1080}, attachmentCurrent: true, nowSeconds: 1000}));
    assert.equal(reduced.frame_comparison, "Resolution reduction planned");
    const old = liveTv.formatLiveTvPictureFacts(liveTv.normalizeLiveTvPictureFacts({
      delivery: null, presentation: {width: 853, height: 480}, attachmentCurrent: false, nowSeconds: 1000}));
    assert.equal(old.stream_frame, "Unavailable");
    assert.equal(old.decode_resolution, "Unavailable");
  });

  await test("startup feedback is immediate and repeated channel presses share one tune", async () => {
    const grant=deferred(),entered=deferred();
    const h=startupHarness({start:()=>{entered.resolve();return grant.promise;}});
    const opening=h.watchLiveTv(0);
    assert.equal(h.nodes["live-tv-host"].hidden,false);
    assert.equal(h.nodes["live-tv-status"].hidden,false);
    assert.equal(h.state.playbackState,"starting");
    assert.match(h.nodes["live-tv-status-text"].textContent,/Tuning 1.1/);
    await entered.promise;
    await h.watchLiveTv(0);
    assert.deepEqual(h.starts,["one"]);
    assert.equal(h.state.serial,1,"a second press does not abandon the first reply");
    grant.resolve({session_id:"cap-one"});await opening;
    assert.equal(h.state.playbackState,"buffering","a resolved play promise is not proof of playback");
    h.video.paused=false;h.video.onplaying();
    assert.equal(h.state.playbackState,"playing");
    assert.equal(h.nodes["live-tv-status"].hidden,true);
    h.resumeLiveTv();
    assert.equal(h.state.playbackState,"playing","Play on an already playing channel does not leave a spinner");
    h.video.onpause();
    assert.equal(h.state.playbackState,"playing","a queued pause from the old attachment is ignored");
    h.video.onwaiting();
    assert.equal(h.state.playbackState,"buffering");
    h.video.onplaying();
    assert.equal(h.nodes["live-tv-status"].hidden,true);
    await h.watchLiveTv(0);
    assert.deepEqual(h.starts,["one"]);
    assert.deepEqual(h.releases,[]);
  });

  await test("autoplay refusal offers Play on the picture without reacquiring the tuner", async () => {
    let denied=true;
    const h=startupHarness({play:()=>denied?Promise.reject({name:"NotAllowedError"}):Promise.resolve()});
    await h.watchLiveTv(0);
    assert.equal(h.state.playbackState,"blocked");
    assert.equal(h.nodes["live-tv-status-play"].hidden,false);
    assert.match(shellSource().html,/id="live-tv-status-play" onclick="resumeLiveTv\(\)"/);
    denied=false;h.resumeLiveTv();
    assert.equal(h.plays,2,"the click calls play synchronously to retain user activation");
    assert.deepEqual(h.starts,["one"]);
    h.video.paused=false;h.video.onplaying();
    assert.equal(h.nodes["live-tv-status"].hidden,true);
    h.pauseLiveTv();
    assert.equal(h.state.playbackState,"paused");
    assert.equal(h.nodes["live-tv-status-play"].hidden,false);
  });

  await test("a failed channel start remains visible and can be retried", async () => {
    let failed=true;
    const h=startupHarness({start:()=>{if(failed)throw {code:"stream_failed"};return {session_id:"retry"};}});
    await h.watchLiveTv(0);
    assert.equal(h.state.playbackState,"error");
    assert.equal(h.nodes["live-tv-status-retry"].hidden,false);
    assert.equal(h.nodes["live-tv-status"].hidden,false);
    failed=false;await h.watchLiveTv(0);
    assert.deepEqual(h.starts,["one","one"]);
    assert.equal(h.lease.current.session_id,"retry");
    assert.equal(h.state.playbackState,"buffering");
  });

  await test("in-picture failures preserve typed retry and alternate-channel actions", async () => {
    const h=startupHarness();
    h.state.channels.push({id:"protected",guide_number:"3",support:"drm",drm:true});
    h.liveTvPlaybackFailure({code:"tuner_capacity",retry:"never",answer:{body:{watchable:[
      {channel_id:"two",guide_number:"2.1"},{channel_id:"protected",guide_number:"3"},
      {channel_id:"gone",guide_number:"4"}]}}});
    assert.equal(h.nodes["live-tv-status-retry"].hidden,true);
    const buttons=h.nodes["live-tv-status-offers"].children;
    assert.deepEqual(buttons.map(button=>button.textContent),["Watch 2.1 instead"]);
    buttons[0].click();
    await settled(0);
    assert.deepEqual(h.starts,["two"]);
    assert.equal(h.nodes["live-tv-status-offers"].children.length,0,"starting clears old alternatives");
    h.liveTvPlaybackFailure({code:"admin_required",status:403});
    assert.equal(h.nodes["live-tv-status-retry"].hidden,true,"a permanent ingress refusal offers no retry");
    h.liveTvPlaybackFailure({code:"stream_failed",retry:"safe"});
    assert.equal(h.nodes["live-tv-status-retry"].hidden,false);
  });

  await test("fullscreen idle separates pointer focus from keyboard and open caption menus", () => {
    let tick,menuOpen=false;
    const classes=new Set(),button={},video={paused:false,ended:false};
    const host={dataset:{mode:"full"},contains:node=>node===button,
      querySelector:()=>menuOpen?{}:null,classList:{add:name=>classes.add(name),remove:name=>classes.delete(name)}};
    const document={activeElement:button,getElementById:()=>video},state={controlsInput:"pointer"};
    const reveal=new Function("LIVE_TV","document","PlaybackPolicy","liveTvHost","setTimeout","clearTimeout",
      shipped("liveTvReveal")+shipped("liveTvIdle")+"return liveTvReveal;")(
      state,document,policy,()=>host,fn=>{tick=fn;return 1;},()=>{});
    reveal();tick();
    assert.ok(classes.has("idle"),"clicking a button does not pin fullscreen controls");
    state.controlsInput="keyboard";reveal();tick();
    assert.ok(!classes.has("idle"),"keyboard focus remains visible");
    document.activeElement=null;reveal();tick();
    assert.ok(classes.has("idle"),"leaving a keyboard control rearms hiding");
    state.controlsInput="pointer";document.activeElement=button;menuOpen=true;reveal();
    const oldTick=tick;tick();
    assert.ok(!classes.has("idle"));assert.notEqual(tick,oldTick,"an open native menu rearms the timer");
    menuOpen=false;tick();assert.ok(classes.has("idle"));
  });

  await test("late Stop teardown cannot erase a newer channel start", async () => {
    const exit=deferred();
    const h=startupHarness({exit:()=>exit.promise});
    await h.watchLiveTv(0);
    const stopping=h.stopLiveTv();
    const opening=h.watchLiveTv(1);
    await opening;
    const loads=h.loads;
    assert.equal(h.video.src,"/api/live-tv/sessions/cap-two/master.m3u8");
    exit.resolve();await stopping;
    assert.equal(h.loads,loads,"the old exit does not detach the new media");
    assert.equal(h.nodes["live-tv-host"].hidden,false);
    assert.equal(h.lease.current.session_id,"cap-two");
    assert.equal(h.state.playbackState,"buffering");
  });

  await test("late autoplay rejection cannot cover a newer channel", async () => {
    let rejectOld;
    const oldPlay=new Promise((_,reject)=>{rejectOld=reject;});
    let attempts=0;
    const h=startupHarness({play:()=>++attempts===1?oldPlay:Promise.resolve()});
    await h.watchLiveTv(0);
    const oldPlaying=h.video.onplaying;
    await h.watchLiveTv(1);
    h.video.paused=false;h.video.onplaying();
    rejectOld({name:"NotAllowedError"});await Promise.resolve();oldPlaying();
    assert.equal(h.state.playbackState,"playing");
    assert.equal(h.nodes["live-tv-status"].hidden,true);
    assert.equal(h.lease.current.session_id,"cap-two");
  });

  await test("duplicate media failures perform only one compatibility retry", async () => {
    const status=deferred();
    const h=startupHarness({status:()=>status.promise});
    await h.watchLiveTv(0);
    h.video.error={code:3};
    const failing=h.video.onerror();const duplicate=h.video.onerror();
    status.resolve({state:"active"});await Promise.all([failing,duplicate]);
    assert.deepEqual(h.starts,["one","one"]);
    assert.deepEqual(h.releases,["cap-one"]);
    assert.equal(h.state.compatibilityRetried,true);
    assert.equal(h.state.playbackState,"buffering");
  });

  await test("playback timeout releases the tuner and keeps an in-picture retry", async () => {
    const h=startupHarness();await h.watchLiveTv(0);
    h.advance(30000);await h.polls[0]();
    assert.equal(h.lease.current,null);
    assert.deepEqual(h.releases,["cap-one"]);
    assert.equal(h.nodes["live-tv-host"].hidden,false);
    assert.equal(h.nodes["live-tv-status-retry"].hidden,false);
    assert.equal(h.state.playbackState,"error");
    assert.match(h.nodes["live-tv-status-text"].textContent,/30 seconds/);
  });

  await test("held channel keys are owned by keyup rather than repeat cadence", () => {
    assert.match(shipped("liveTvChangeChannel"), /LIVE_TV_CHANNEL_GESTURE\.press\(key,LIVE_TV\)/);
    assert.match(shipped("liveTvWireKeys"), /keyup[\s\S]*LIVE_TV_CHANNEL_GESTURE\.release\(event\.key,LIVE_TV\)/);
    assert.match(shipped("liveTvWireKeys"), /blur[\s\S]*LIVE_TV_CHANNEL_GESTURE\.blur\(LIVE_TV\)/);
    assert.match(shipped("liveTvSelect"), /cancelLiveTvChannelGesture\(\)/,
      "a direct tune cancels a pending held-key preview");
    assert.match(shipped("stopLiveTv"), /cancelLiveTvChannelGesture\(\)/,
      "Stop cancels the old physical key owner before releasing the tuner");
  });

  await test("stopping a held channel gesture cannot reacquire a tuner on keyup", () => {
    let now=0,nextTimer=1;
    const timers=new Map(),watched=[];
    const setTimer=(fn,ms)=>{const id=nextTimer++;timers.set(id,{at:now+ms,fn});return id;};
    const clearTimer=id=>timers.delete(id);
    const advance=to=>{
      while(true){
        const due=Array.from(timers.entries()).filter(([,timer])=>timer.at<=to)
          .sort((a,b)=>a[1].at-b[1].at||a[0]-b[0])[0];
        if(!due) break;
        timers.delete(due[0]);now=due[1].at;due[1].fn();
      }
      now=to;
    };
    const state={channels:[{id:"a"},{id:"b"},{id:"c"}],selected:"a",
      pendingChannel:null,preview:null};
    const harness=new Function("PlaybackPolicy","PlurxLiveTv","LIVE_TV","setTimer","clearTimer",
      "liveTvVisible","liveTvPaint","liveTvSetPref","watchLiveTv","renderLiveTvChannels",[
        "const LIVE_TV_CHANNEL_GESTURE=PlaybackPolicy.createHeldKeyCommitter({"+
          "delayMs:PlaybackPolicy.liveContractTiming('channel_coalesce_ms'),"+
          "commit:owner=>{if(owner!==LIVE_TV)return;const pending=LIVE_TV.pendingChannel;"+
          "LIVE_TV.pendingChannel=null;LIVE_TV.preview=null;if(pending)liveTvSelect(pending);},"+
          "setTimer,clearTimer});",
        shipped("cancelLiveTvChannelGesture"),
        shipped("liveTvSelect"),
        shipped("liveTvChangeChannel"),
        "return {change:liveTvChangeChannel,cancel:cancelLiveTvChannelGesture,"+
          "release:key=>LIVE_TV_CHANNEL_GESTURE.release(key,LIVE_TV)};",
      ].join("\n"))(policy,liveTv,state,setTimer,clearTimer,()=>state.channels,()=>{},()=>{},
        index=>watched.push(index),()=>{});

    harness.change(1,"ArrowDown");advance(350);
    harness.cancel();harness.release("ArrowDown");advance(1_000);
    assert.deepEqual(watched,[],"the keyup after Stop starts no tuner session");

    harness.change(1,"ArrowDown");advance(1_100);
    harness.change(1,"ArrowDown");harness.release("ArrowDown");advance(1_450);
    assert.deepEqual(watched,[2],"an uninterrupted held gesture starts one final channel");
  });

  await test("both guide views put the compact player above a full-width player-height guide", () => {
    const list = shipped("liveTvListMarkup"), grid = shipped("liveTvGridMarkup");
    const stage = shipped("liveTvStageMarkup"), now = shipped("liveTvNowBar");
    assert.ok(list.indexOf("liveTvStageMarkup") < list.indexOf('class="lt-list"'));
    assert.ok(grid.indexOf("liveTvStageMarkup") < grid.indexOf('class="lt-gridwrap"'));
    assert.doesNotMatch(grid, /grid-template-columns:minmax\(0,1fr\) 400px/);
    assert.match(shell, /\.lt-stage-top\{[^}]*grid-template-columns:minmax\(0,2fr\) minmax\(240px,1fr\)/s);
    assert.match(stage, /lt-showinfo|liveTvShowInfo/);
    assert.match(stage, /lt-stage\$\{wide\?" wide":""\}/);
    assert.match(now, /Larger/);
    assert.match(now, /Smaller/);
    assert.match(grid, /liveTvFormatBadges/);
    assert.match(now, /liveTvTechnicalDetails/);
    for(const control of ["pauseLiveTv", "muteLiveTv", "toggleLiveTvPip", "fullscreenLiveTv", "stopLiveTv"])
      assert.match(now, new RegExp(control));
    assert.match(shell, /\.lt-list\{[^}]*overflow:auto[^}]*height:var\(--live-tv-slot-height,64vh\)/s);
    assert.match(shell, /\.lt-gridwrap\{[^}]*height:var\(--live-tv-slot-height,64vh\)/s);
    assert.match(shipped("liveTvTrackSlot"), /--live-tv-slot-height/);
  });

  await test("nothing in the shipped page can refuse to start a channel", () => {
    // The barrier, its quarantine code, and the two classification sets that
    // decided who was allowed to press Watch are gone. What replaced them is
    // one reducer over the server's answer, and it has no refusal in it.
    assert.doesNotMatch(shell, /start_outcome_unknown/);
    assert.doesNotMatch(shell, /DEFINITIVE_REFUSALS/);
    assert.doesNotMatch(shell, /StartBarrier/);
    assert.doesNotMatch(shell, /another tab is watching/);
    assert.equal(typeof liveTv.StartBarrier, "undefined");
    assert.equal(typeof liveTv.StartHints, "function");
    const source = fs.readFileSync(
      path.join(__dirname, "../../crates/plurxd/src/web/live-tv.js"), "utf8");
    assert.doesNotMatch(source, /start_outcome_unknown/);
    assert.doesNotMatch(source, /live_tv_storage_unavailable/,
      "a browser that will not keep a hint still watches television");
  });

  await test("mute stays an icon while its accessible action follows player state", () => {
    const video = { muted: false };
    const buttons = [
      { textContent: "🔇", title: "Mute", setAttribute(name, value) { this[name] = value; } },
      { textContent: "🔇", title: "Mute", setAttribute(name, value) { this[name] = value; } },
    ];
    const toggle = new Function("document",
      `${shipped("liveTvSyncMuteButtons")}${shipped("muteLiveTv")} return muteLiveTv;`)(
      { getElementById: id => id === "live-tv-video" ? video : null,
        querySelectorAll: selector => selector === "[data-live-tv-mute]" ? buttons : [] });
    toggle();
    assert.equal(video.muted, true);
    assert.deepEqual(buttons.map(button => [button.textContent, button.title, button["aria-label"]]),
      [["🔊", "Unmute", "Unmute"], ["🔊", "Unmute", "Unmute"]]);
    toggle();
    assert.equal(video.muted, false);
    assert.deepEqual(buttons.map(button => [button.textContent, button.title, button["aria-label"]]),
      [["🔇", "Mute", "Mute"], ["🔇", "Mute", "Mute"]]);
  });

  await test("live captions select the media track in every player geometry", () => {
    const tracks=[
      {kind:"captions",label:"English 708",language:"en",mode:"hidden"},
      {kind:"captions",label:"English CC1",language:"en",mode:"hidden"},
      {kind:"metadata",label:"timing",language:"",mode:"hidden"},
    ];
    const select={dataset:{},innerHTML:"",value:"off",parentElement:{hidden:true}};
    const transport={dataset:{},innerHTML:"",value:"off",parentElement:{hidden:true}};
    const document={getElementById:id=>id==="live-tv-video"?{textTracks:tracks}
      :id==="live-tv-captions"?select:null,querySelectorAll:()=>[select,transport]};
    const controls=new Function("document","esc",[
      shipped("liveTvCaptionTracks"),shipped("liveTvCaptionTrackLabel"),
      shipped("liveTvRefreshCaptionControls"),shipped("liveTvSelectCaption"),
      "return {refresh:liveTvRefreshCaptionControls,select:liveTvSelectCaption};",
    ].join("\n"))(document,String);
    controls.refresh();
    assert.match(select.innerHTML,/English 708/);
    assert.match(select.innerHTML,/English CC1/);
    assert.doesNotMatch(select.innerHTML,/timing/);
    assert.equal(select.value,"off");
    controls.select("0");
    assert.deepEqual(tracks.map(track=>track.mode),["showing","disabled","hidden"]);
    assert.equal(select.value,"0");
    controls.select("1");
    assert.deepEqual(tracks.map(track=>track.mode),["disabled","showing","hidden"]);
    assert.equal(select.value,"1");
    assert.equal(transport.value,"1");
    assert.equal(transport.parentElement.hidden,false);
    controls.select("off");
    assert.deepEqual(tracks.map(track=>track.mode),["disabled","disabled","hidden"]);
    assert.equal(select.value,"off");
    assert.match(shellSource().html,/id="live-tv-captions"[^>]*aria-label="Live TV captions"/);
    assert.match(shipped("liveTvNowBar"),/data-live-tv-captions/);
    assert.match(shell,/\.lth\[data-mode="slot"\] \.lth-captions\{display:none\}/);
    assert.match(shell,/\.lth\.idle \.lth-captions\{opacity:0/);
    tracks.splice(0);
    controls.refresh();
    assert.equal(select.parentElement.hidden,true);
    assert.equal(transport.parentElement.hidden,true);
    assert.match(shipped("liveTvStatsTelemetry"),/selectedCaption\?liveTvCaptionTrackLabel/);
  });

  await test("protected channels are visible but never watchable", () => {
    assert.deepEqual(liveTv.channelView({ drm: true, support: "drm_unsupported" }), {
      disabled: true,
      label: "Protected · unsupported",
      tone: "warn",
    });
    assert.deepEqual(liveTv.channelView({ drm: false, support: "ready" }), {
      disabled: false,
      label: "Available",
      tone: "ready",
    });
  });

  await test("lineup format facts become compact badges without guessing", () => {
    assert.deepEqual(liveTv.channelBadges({ hd: true, video_codec: "hevc", audio_codec: "ac4" }),
      ["HD", "HEVC", "AC4"]);
    assert.deepEqual(liveTv.channelBadges({ hd: false, video_codec: " h264 ", audio_codec: "H264" }),
      ["SD", "H264"]);
    assert.deepEqual(liveTv.channelBadges({}), []);
    assert.deepEqual(liveTv.channelBadges({
      hd: true, video_codec: "hevc", audio_codec: "ac3",
      source_format: {
        video_width: 3840, video_height: 2160, scan: "progressive",
        audio_channels: 6, audio_layout: "5.1", observed_at: 1788998400,
      },
    }, 1788998500), ["4K", "HEVC", "AC3 5.1"]);
    assert.deepEqual(liveTv.sourceDetails({
      video_codec: "mpeg2video", audio_codec: "ac3",
      source_format: {
        video_width: 1920, video_height: 1080, scan: "interlaced",
        audio_channels: 2, audio_layout: "stereo", observed_at: 1788998400,
      },
    }, 1788998500), {
      compact: ["HD", "MPEG2VIDEO", "AC3 Stereo"],
      exact: ["1920×1080i", "MPEG2VIDEO", "AC3 Stereo"],
      observedAt: 1788998400,
    });
    assert.deepEqual(liveTv.channelBadges({
      hd: false,
      source_format: {
        video_width: -1, video_height: 2160, scan: "made-up",
        audio_channels: 6, audio_layout: {}, observed_at: 1788998400,
      },
    }, 1788998500), ["4K", "6 ch"]);
    assert.deepEqual(liveTv.channelBadges({
      hd: true,
      source_format: { video_height: 2160, observed_at: 1788998400 },
    }, 1788999600), ["HD"]);
    assert.deepEqual(liveTv.channelBadges({
      hd: false,
      source_format: { video_height: 2160, observed_at: 1788998400 },
    }, 1788998500, 1788998450), ["SD"]);

    const details = new Function("PlurxLiveTv", "esc",
      `${shipped("liveTvTechnicalDetails")} return liveTvTechnicalDetails;`)(liveTv, String);
    const markup = details(
      { hd: true, video_codec: "HEVC", audio_codec: "AC4" },
      { encoder: "vaapi", output_height: 720,
        signal: { strength_percent: 96, quality_percent: 89, symbol_quality_percent: 100 } },
    );
    assert.match(markup, /Source[\s\S]*HD · HEVC · AC4/);
    assert.match(markup, /Stream format[\s\S]*H\.264 · 720p · AAC · VAAPI encoder/);
    assert.match(markup, /Strength[\s\S]*96%[\s\S]*Quality[\s\S]*89%[\s\S]*Symbol[\s\S]*100%/);
  });

  await test("typed terminal failures produce specific recovery", () => {
    assert.deepEqual(liveTv.errorView({ code: "capability_expired" }), {
      code: "capability_expired",
      title: "Live session expired",
      detail: "The player was idle or disconnected. Start the channel again.",
      retryable: true,
      offers: [],
    });
    assert.equal(liveTv.errorView({ code: "stream_failed" }).title, "Live stream stopped");
    assert.equal(liveTv.errorView({ code: "owner_unavailable" }).retryable, true);
    // The server's own advice drives the button now, not a table in here.
    assert.equal(liveTv.errorView({ code: "drm_unsupported", retry: "never" }).retryable, false);
    assert.equal(liveTv.errorView({ code: "invalid_request", status: 400 }).retryable, false,
      "a refusal the ingress made offers no retry even when it advises nothing");
    assert.equal(liveTv.errorView({ code: "unknown-server-code" }).title, "Tuner owner unavailable");
    assert.equal(liveTv.errorView({ code: "no_answer" }).title, "The server did not answer");
    assert.equal(liveTv.errorView({ code: "no_answer" }).detail, "Press the channel again.");
  });

  await test("close and channel switch release each capability exactly once", async () => {
    const releases = [];
    const lease = new liveTv.Lease({
      start: async (channel) => ({ session_id: `cap-${channel}`, channel: { id: channel } }),
      release: async (id) => { releases.push(id); },
    });
    assert.equal((await lease.start("7.1")).session_id, "cap-7.1");
    assert.equal((await lease.start("9.1")).session_id, "cap-9.1");
    await Promise.all([lease.stop(), lease.stop()]);
    assert.deepEqual(releases, ["cap-7.1", "cap-9.1"]);
  });

  await test("a superseded late start is released before the next tuner opens", async () => {
    const first = deferred();
    const started = deferred();
    const events = [];
    const lease = new liveTv.Lease({
      start: async (channel) => {
        events.push(`start:${channel}`);
        if (channel === "2.1") {
          started.resolve();
          return first.promise;
        }
        return { session_id: `cap-${channel}` };
      },
      release: async (id) => { events.push(`release:${id}`); },
    });
    const opening = lease.start("2.1");
    await started.promise;
    const switched = lease.start("4.1");
    first.resolve({ session_id: "cap-2.1" });
    assert.equal(await opening, null);
    assert.equal((await switched).session_id, "cap-4.1");
    assert.deepEqual(events, ["start:2.1", "release:cap-2.1", "start:4.1"]);
  });

  await test("status and keepalive cannot commit across a switch", async () => {
    const status = deferred();
    const lease = new liveTv.Lease({
      start: async (channel) => ({ session_id: `cap-${channel}` }),
      release: async () => {},
      status: async () => status.promise,
      keepalive: async () => ({ ok: true }),
    });
    await lease.start("7.1");
    const stale = lease.status();
    const switched = lease.start("8.1");
    status.resolve({ state: "active" });
    assert.equal(await stale, null);
    await switched;
    assert.deepEqual(await lease.keepalive(), { ok: true });
  });

  await test("a status poll never renews a capability that is being released", async () => {
    const release = deferred();
    let statuses = 0;
    const lease = new liveTv.Lease({
      start: async (channel) => ({ session_id: `cap-${channel}` }),
      release: async () => release.promise,
      status: async () => { statuses++; return { state: "active" }; },
      keepalive: async () => ({ ok: true }),
    });
    await lease.start("7.1");
    const closing = lease.stop();
    // `stop()` bumps the generation synchronously but only sets `releasing` two
    // microtasks later. A poll entered in that window is the real hazard, so it
    // is checked BEFORE letting the chain run — waiting first would hide it.
    const racing = lease.status();
    assert.equal(await racing, null, "a poll entered before the DELETE begins must be refused");
    assert.equal(statuses, 0, "a superseded capability must never be renewed");
    await new Promise((settle) => setImmediate(settle));
    assert.equal(lease.releasing, true, "the DELETE must still be in flight");
    assert.equal(await lease.status(), null);
    assert.equal(statuses, 0, "a release in flight must not renew its own lease");
    assert.equal(await lease.keepalive(), null);
    release.resolve();
    await closing;
    assert.equal(lease.current, null);
  });

  await test("failed cleanup retains the capability and blocks another tuner", async () => {
    const events = [];
    let fail = true;
    const lease = new liveTv.Lease({
      start: async (id) => { events.push(`start:${id}`); return { session_id: id }; },
      release: async (id) => { events.push(`release:${id}`); if (fail) throw new Error("offline"); },
    });
    await lease.start("one");
    await assert.rejects(lease.start("two"), /offline/);
    assert.equal(lease.current.session_id, "one");
    assert.deepEqual(events, ["start:one", "release:one"]);
    fail = false;
    await lease.start("three");
    assert.deepEqual(events, ["start:one", "release:one", "release:one", "start:three"]);
    await lease.stop();
    assert.equal(lease.current, null);
  });

  await test("late-start cleanup failure is retained across close", async () => {
    const pending = deferred(), started = deferred();
    let fail = true;
    const lease = new liveTv.Lease({
      start: async () => { started.resolve(); return pending.promise; },
      release: async () => { if (fail) throw new Error("offline"); },
    });
    const opening = lease.start("one");
    await started.promise;
    const closing = lease.stop();
    pending.resolve({ session_id: "one" });
    await assert.rejects(opening, /offline/);
    await assert.rejects(closing, /offline/);
    assert.equal(lease.current.session_id, "one");
    fail = false;
    await lease.stop();
    assert.equal(lease.current, null);
  });

  await test("autoplay rejection releases the tuner within the play deadline", async () => {
    let clock = 1000, poll, releases = 0, statuses = 0;
    const video = { paused: true, canPlayType: () => "maybe", play: async () => { throw new Error("autoplay denied"); } };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    // `support` is a non-optional enum on the wire, so a fixture channel that
    // omits it is not a channel this client can ever receive.
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => { statuses++; return { state: "active" }; }, keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")}${shipped("liveTvAttachSession")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 6; i++) { clock += 10000; await poll(); }
    assert.equal(statuses, 0, "paused status requests also renew server leases");
    assert.equal(releases, 1);
    assert.equal(lease.current, null);
    assert.equal(nodes["live-tv-host"].hidden, true);
  });

  await test("the tuner watchdog, the keepalive and the status poll all survive docking", async () => {
    // The whole point of M3 is that leaving the route keeps the picture. The
    // lease keeps the household's only tuner, so the three things that own it
    // must keep running in exactly that state — and the poll's liveness gate
    // used to read the route and the render generation, both of which change
    // the instant you dock. Nothing here asserts source text: it drives the
    // real poll across a real route change.
    let clock = 1000, poll, keepalives = 0, statuses = 0, releases = 0, frames = 240;
    const video = { paused: false, currentTime: 5, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: frames }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true, dataset: {} }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => { statuses++; return { state: "active" }; },
      keepalive: async () => { keepalives++; },
    });
    const routing = { hash: "#/live-tv", generation: 1 };
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv", "ROUTING",
      `const location=ROUTING, PLAYER=null, API='/api', window={};
       let PAGE_RENDER_GENERATION=ROUTING.generation;
       function detachLiveTvMedia(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){} function liveTvSetMode(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")}${shipped("liveTvAttachSession")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv, routing);

    await run(0);
    assert.equal(video.src, "/api/live-tv/sessions/cap/master.m3u8",
      "the attached player reads caption declarations from the master playlist");
    clock += 10000; frames += 60; await poll();
    assert.equal(keepalives, 1, "the poll runs on the page");

    // Dock: the viewer navigates to another route. Both liveness inputs move.
    routing.hash = "#/";
    for (let i = 0; i < 12; i++) { clock += 10000; frames += 60; await poll(); }
    assert.equal(keepalives, 13, "a docked stream still renews the lease it holds");
    assert.equal(statuses, 13, "and still asks the owner whether the session is alive");

    // And the 30-second no-progress release still fires while docked, which is
    // the difference between a stalled dock and a permanently held tuner.
    for (let i = 0; i < 5; i++) { clock += 10000; await poll(); }
    assert.equal(releases, 1, "a docked stream that stops decoding gives the tuner back");
    assert.equal(lease.current, null);
  });

  await test("navigating away during the start POST still docks and can still be stopped", async () => {
    // Before: `liveTvLeaveRoute` docked only when a lease already existed, and
    // during the ~45 s start there is none — so the session landed with no
    // media, no poll and no Stop control, holding a tuner invisibly.
    let clock = 1000, poll, releases = 0;
    const started = deferred(), grant = deferred();
    const video = { paused: false, currentTime: 5, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 240 }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true, dataset: {} }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0, starting: null };
    const lease = new liveTv.Lease({
      start: async () => { started.resolve(); return grant.promise; },
      release: async () => { releases++; },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const routing = { hash: "#/live-tv" };
    const modes = [];
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv", "ROUTING", "MODES",
      `const location=ROUTING, PLAYER=null, API='/api', window={};
       let PAGE_RENDER_GENERATION=1;
       function detachLiveTvMedia(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvSetMode(mode){ MODES.push(mode); }
       function liveTvHost(){ return document.getElementById("live-tv-host"); }
       function liveTvShowHost(){ MODES.push("slot"); } function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("liveTvLeaveRoute")}${shipped("watchLiveTv")}${shipped("liveTvAttachSession")}
       return {watchLiveTv,liveTvLeaveRoute};`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv, routing, modes);

    const watching = run.watchLiveTv(0);
    await started.promise;
    assert.equal(state.starting, 1, "a start in flight is visible to the router");

    // The viewer leaves while the POST is outstanding.
    routing.hash = "#/";
    run.liveTvLeaveRoute();
    assert.deepEqual(modes, ["slot", "dock"], "startup is visible immediately and docks when leaving");

    grant.resolve({ session_id: "cap" });
    await watching;
    assert.equal(lease.current.session_id, "cap");
    assert.ok(poll, "the granted session is wired to a poll it can be released by");
    assert.equal(state.starting, null, "and the flag is cleared once the start settles");

    // The watchdog can now reclaim it, which is what makes the dock safe.
    video.paused = true;
    for (let i = 0; i < 5; i++) { clock += 10000; await poll(); }
    assert.equal(releases, 1);
  });

  await test("starting a channel closes an open film and reports only that film's progress", async () => {
    // The one line on the Live TV path that can touch VOD state is
    // `closePlayer()`, and the browser acceptance never opens a film, so this
    // is where that path is pinned. The shipped `closePlayer` does report
    // progress — for the file being closed, which is the viewer's own position
    // in the film they were watching, not Live TV writing VOD state. Losing it
    // would be the defect; what must not happen is a second close, a close
    // after the tuner is open, or a report for anything else.
    let clock = 1000, poll, closes = 0;
    const progress = [];
    const video = { paused: false, currentTime: 5, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 240 }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const events = [];
    const lease = new liveTv.Lease({
      start: async (id) => { events.push("start:" + id); return { session_id: "cap" }; },
      release: async () => { events.push("release"); },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval",
      "PlurxLiveTv", "PLAYER", "closePlayer", "reportProgress",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")}${shipped("liveTvAttachSession")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv,
      { fileId: 7 }, () => { closes += 1; progress.push(7); }, (id) => progress.push(id));
    await run(0);
    assert.equal(closes, 1, "an open film must be closed exactly once before a tuner opens");
    assert.deepEqual(progress, [7], "only the closing film's own progress may be reported");
    assert.deepEqual(events, ["start:one"], "and the tuner must still open afterwards");
  });

  await test("a live window that moves backwards never stops a decoding stream", async () => {
    let clock = 1000, poll, releases = 0, frames = 0;
    // Healthy live playback whose position regresses as the window slides.
    const video = { paused: false, currentTime: 40, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: frames }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")}${shipped("liveTvAttachSession")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 6; i++) { clock += 10000; frames += 120; video.currentTime -= 4; await poll(); }
    assert.equal(releases, 0, "decoded frames advanced, so the stream was never stuck");
    assert.notEqual(lease.current, null);
  });

  await test("a frozen picture still expires even while position climbs", async () => {
    let clock = 1000, poll, releases = 0;
    // The mirror image: the timeline advances but nothing is decoded.
    const video = { paused: false, currentTime: 10, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 500 }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")}${shipped("liveTvAttachSession")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 5; i++) { clock += 10000; video.currentTime += 10; await poll(); }
    assert.equal(releases, 1, "a frozen decoder must not be renewed by a moving clock");
    assert.equal(lease.current, null);
  });

  // ---- the hint store, the press flow, and what decides a retirement ------
  // tests/playback/live-tv-start-cases.json is the same fixture the Apple and
  // Android leases read: one reducer, three clients, and never a refusal.
  const START_CASES = JSON.parse(
    fs.readFileSync(path.join(__dirname, "../playback/live-tv-start-cases.json"), "utf8"),
  );
  const CONTRACT_TIMINGS = JSON.parse(
    fs.readFileSync(path.join(__dirname, "../playback/player-input-contract.json"), "utf8"),
  ).live.timings;

  const LEASE_BLOCK = shell.slice(
    shell.indexOf("const LIVE_TV="), shell.indexOf("async function liveTvRequest("));

  // One shared bus, so a test can stand a second document next to the one
  // under test and have it answer the liveness probe for real.
  function tabBus() {
    const ports = [];
    return class Channel {
      constructor() { this.listeners = []; this.onmessage = null; ports.push(this); }
      addEventListener(_name, listener) { this.listeners.push(listener); }
      removeEventListener(_name, listener) {
        this.listeners = this.listeners.filter(entry => entry !== listener);
      }
      postMessage(data) {
        for (const port of ports) {
          if (port === this) continue;
          if (port.onmessage) port.onmessage({ data });
          for (const listener of port.listeners.slice()) listener({ data });
        }
      }
    };
  }

  // The shipped lease, its hint store and its retire path, over fakes. This is
  // the real source from index.html, not a copy of its rules.
  function shippedLease(options) {
    const settings = options || {};
    const storage = settings.storage || memoryStorage();
    const wall = settings.wall || (() => 1_000_000);
    let nonce = 0;
    const scope = new Function(
      "PlurxLiveTv", "PlaybackPolicy", "performance", "Date", "localStorage", "crypto",
      "liveTvRequest", "BroadcastChannel", "currentCapsDocument",
      `${LEASE_BLOCK}
       return {LIVE_TV,LIVE_TV_LEASE,LIVE_TV_HINTS,LIVE_TV_TABS,liveTvRequestId,liveTvRetireOrphanHints};`,
    )(
      liveTv, policy, { now: () => 0 }, { now: wall }, storage,
      { getRandomValues: bytes => { for (let i = 0; i < bytes.length; i++) bytes[i] = (++nonce) & 255; return bytes; } },
      settings.request || (async () => ({ session_id: "cap", live: true })),
      settings.channel || function () { throw new Error("no BroadcastChannel"); },
      () => ({ video: [{ codec: "h264", profiles: ["high"], max_height: 1080 }], audio: ["aac"] }),
    );
    scope.LIVE_TV.protocols = "protocols" in settings ? settings.protocols : [1, 2, 3];
    scope.storage = storage;
    return scope;
  }

  // The shipped open-time resume, with its real liveness probe attached.
  function shippedResume(options) {
    const settings = options || {};
    const storage = settings.storage || memoryStorage();
    const wall = settings.wall || (() => 1_000_000);
    const attached = [], asked = [];
    const state = Object.assign({
      channels: [{ id: "7.1" }], serial: 0, protocols: [1, 2, 3],
      lineupRead: true, resumed: false, hint: null, starting: null,
    }, settings.state || {});
    const hints = new liveTv.StartHints(() => storage, wall);
    const lease = settings.lease || new liveTv.Lease({
      start: async () => ({ session_id: "unused" }), release: async () => {},
    });
    const resume = new Function(
      "PlurxLiveTv", "PlaybackPolicy", "LIVE_TV", "LIVE_TV_HINTS", "LIVE_TV_LEASE", "LIVE_TV_TABS",
      "liveTvRequest", "Date", "ATTACHED",
      `const PAGE_RENDER_GENERATION=1, location={hash:"#/live-tv"};
       async function liveTvAttachSession(info,index,serial,generation){ ATTACHED.push({info,index}); }
       ${shipped("liveTvRecoveryEnabled")}${shipped("liveTvAnswer")}${shipped("liveTvOrphanHints")}${shipped("liveTvResumeStart")}
       return liveTvResumeStart;`,
    )(
      liveTv, policy, state, hints, lease, settings.tabs || null,
      async (path, method) => { asked.push(`${method} ${path}`); return settings.answer(path); },
      { now: wall }, attached,
    );
    return { resume, attached, asked, hints, lease, state, storage };
  }

  // The shape the fixture's `answers` rows put on the wire.
  function answerRequest(rows, thrown) {
    return async (path, method, _timeout, _auth, _keepalive, body) => {
      rows.push({ path, method, body });
      if (method !== "POST" || !path.endsWith("/sessions")) return { outcome: "retired" };
      throw thrown();
    };
  }

  function thrownFor(rule) {
    if (rule.transport === "timeout") return new Error("the request did not answer");
    const error = new Error("refused");
    error.status = rule.status || 503;
    error.code = rule.body && rule.body.code;
    error.answer = { transport: "http", status: error.status, body: rule.body || {} };
    return error;
  }

  await test("the start reducer answers every shared start case", () => {
    for (const rule of START_CASES.answers) {
      const answer = rule.transport === "timeout"
        ? { transport: "timeout" }
        : { status: rule.status, body: rule.body };
      const outcome = liveTv.startOutcome(answer);
      assert.equal(outcome.render, rule.render, `${rule.case}: render`);
      assert.equal(outcome.offerRetry, rule.offer_retry, `${rule.case}: offer_retry`);
      assert.equal(outcome.keepHint, rule.keep_hint, `${rule.case}: keep_hint`);
      assert.equal(outcome.replay, rule.replay === true, `${rule.case}: replay`);
      // Every key the fixture can render has copy, and the copy's own retry
      // control agrees with the reducer that produced it.
      const body = rule.body || {};
      const view = liveTv.errorView({
        code: outcome.render, status: rule.status, retry: body.retry, owner_decided: body.owner_decided,
      });
      assert.ok(view.title && view.detail, `${rule.case}: ${outcome.render} has no copy`);
      assert.equal(view.retryable, rule.offer_retry, `${rule.case}: the view's retry control`);
    }
  });

  await test("capacity offers the shared fixture's watchable channels", () => {
    const rule = START_CASES.answers.find(row => row.offer_watchable);
    assert.ok(rule, "the shared fixture needs a watchable capacity case");
    const error = { code: rule.body.code, status: 503, retry: rule.body.retry,
      owner_decided: rule.body.owner_decided, answer: { body: rule.body } };
    assert.deepEqual(liveTv.errorView(error).offers.map(offer => offer.channelId), rule.offer_watchable);
    assert.deepEqual(liveTv.errorView(error).offers.map(offer => offer.label),
      ["Watch 2.1 instead", "Watch 4.1 instead"]);
    assert.deepEqual(liveTv.errorView({ code: "tuner_unavailable", answer: { body: rule.body } }).offers, []);
  });

  await test("an ingress code is only the ingress's verdict when the status is a 4xx", async () => {
    // The same four codes can arrive from either side. A 4xx is the ingress
    // rejecting the request before an owner saw it — nothing to retire. In a
    // 5xx the request was on its way to, or already at, an owner that may have
    // opened a tuner, so the handle has to survive. Apple rules it on
    // 400..<500 and web now matches.
    const ingress = liveTv.startOutcome({ status: 400, body: { code: "invalid_request" } });
    assert.equal(ingress.keepHint, false);
    assert.equal(ingress.offerRetry, false);
    const enRoute = liveTv.startOutcome({ status: 502, body: { code: "invalid_request" } });
    assert.equal(enRoute.keepHint, true,
      "a 5xx carrying an ingress code and no verdict keeps the only handle to the start");
    assert.equal(enRoute.offerRetry, true, "and the viewer can press again");
    assert.equal(enRoute.render, "invalid_request", "the copy still names what the server said");
    // A 5xx that an owner did decide still forgets it, on the verdict alone.
    assert.equal(liveTv.startOutcome({
      status: 503, body: { code: "live_tv_disabled", retry: "never", owner_decided: true },
    }).keepHint, false);

    // And through the shipped press, where the status comes off the wire.
    const rows = [];
    const lease = shippedLease({ request: answerRequest(rows, () => {
      const error = new Error("bad gateway");
      error.status = 502; error.code = "admin_required";
      error.answer = { transport: "http", status: 502, body: { code: "admin_required" } };
      return error;
    }) });
    await assert.rejects(lease.LIVE_TV_LEASE.start("7.1"), error => error.code === "admin_required");
    assert.equal(lease.LIVE_TV_HINTS.list().length, 1,
      "the press keeps the handle for a refusal no owner is known to have made");
  });

  await test("the shipped press keeps, forgets and replays exactly as the fixture rules", async () => {
    for (const rule of START_CASES.answers) {
      const rows = [];
      const lease = shippedLease({ request: answerRequest(rows, () => thrownFor(rule)) });
      await assert.rejects(lease.LIVE_TV_LEASE.start("7.1"), error => {
        assert.equal(error.code, rule.render, `${rule.case}: rendered code`);
        if(rule.offer_watchable){
          assert.deepEqual(liveTv.errorView(error).offers.map(offer => offer.channelId),
            rule.offer_watchable, `${rule.case}: offers survive the shipped press`);
        }
        return true;
      });
      const posts = rows.filter(row => row.method === "POST" && row.path.endsWith("/sessions"));
      assert.equal(posts.length, rule.replay === true ? 2 : 1, `${rule.case}: POSTs sent`);
      if (rule.replay === true) {
        assert.equal(posts[0].body.request_id, posts[1].body.request_id,
          `${rule.case}: a replay carries the same id, or it opens a second tuner`);
      }
      assert.equal(lease.LIVE_TV_HINTS.list().length, rule.keep_hint ? 1 : 0,
        `${rule.case}: hints kept`);
    }
  });

  await test("resume decides every shared open-time case, and never tunes", async () => {
    for (const rule of START_CASES.resume) {
      const storage = memoryStorage();
      storage.setItem("plurx_live_tv_hint_v1:abc", "1000"); // long unclaimed
      const run = shippedResume({
        storage,
        answer: () => { if (rule.transport === "timeout") throw new Error("no answer"); return rule.body; },
      });
      const { attached, hints, lease } = run;
      await run.resume(1, "#/live-tv");
      const kept = hints.list().length === 1;
      if (rule.then === "reattach") {
        assert.equal(attached.length, 1, `${rule.case}: the session is attached`);
        assert.equal(attached[0].info.session_id, rule.body.session.session_id, rule.case);
        assert.equal(attached[0].index, 0,
          `${rule.case}: the owner's session names the channel it is on`);
        assert.ok(attached[0].info.playlist_url && attached[0].info.channel,
          `${rule.case}: the whole activation is handed to the attach path`);
        assert.equal(lease.current.session_id, rule.body.session.session_id,
          `${rule.case}: the lease owns what it rejoined`);
        assert.ok(kept, `${rule.case}: the handle stays until a DELETE confirms`);
      } else {
        assert.equal(attached.length, 0, `${rule.case}: nothing is tuned on open`);
        assert.equal(kept, rule.then === "keep_hint_wait", `${rule.case}: ${rule.then}`);
      }
    }
  });

  await test("a second tab never rejoins the session the first tab is watching", async () => {
    // Two tabs, one shared localStorage. Tab A is watching; tab B opens Live
    // TV and finds A's hint. Rejoining it would put both documents on one
    // capability, and the first of them to leave would DELETE it — A's picture
    // would stop mid-programme with capability_expired. B must not ask.
    const Channel = tabBus(), storage = memoryStorage(), wall = 1_000_000;
    const watching = shippedLease({
      storage, wall: () => wall, channel: Channel,
      request: async (path, method) => method === "POST" && path.endsWith("/sessions")
        ? { session_id: "cap-a", live: true } : { outcome: "retired" },
    });
    await watching.LIVE_TV_LEASE.start("7.1");
    const held = watching.LIVE_TV.hint;
    // Old on the wall clock: only being alive can save it.
    storage.setItem(`plurx_live_tv_hint_v1:${held}`, String(wall - 600_000));

    const second = shippedResume({
      storage, wall: () => wall, tabs: new Channel(),
      answer: () => ({ outcome: "live", session: { session_id: "cap-a", live: true,
        playlist_url: "/api/v1/live-tv/sessions/cap-a/master.m3u8", channel: { id: "7.1" } } }),
    });
    await second.resume(1, "#/live-tv");
    assert.deepEqual(second.asked, [],
      "a hint a live document claims is not this document's to resume");
    assert.deepEqual(second.attached, [], "and nothing is adopted");
    assert.equal(second.lease.current, null);
    assert.equal(watching.LIVE_TV_LEASE.current.session_id, "cap-a",
      "the watching tab still holds the only handle on its session");
    assert.ok(storage.getItem(`plurx_live_tv_hint_v1:${held}`) !== null, "and keeps its hint");

    // The same hint, once nothing answers for it, is resumable again.
    const later = shippedResume({
      storage, wall: () => wall, tabs: new Channel(),
      answer: () => ({ outcome: "live", session: { session_id: "cap-a", live: true,
        playlist_url: "/api/v1/live-tv/sessions/cap-a/master.m3u8", channel: { id: "7.1" } } }),
    });
    watching.LIVE_TV.hint = null; // the watching document is gone
    await later.resume(1, "#/live-tv");
    assert.deepEqual(later.asked, [`POST /live-tv/starts/${held}/resume`],
      "an unclaimed session is exactly what resume is for");
    assert.equal(later.attached.length, 1);
  });

  await test("a lineup read that failed leaves the resume for the next visit", async () => {
    // The app was killed mid-stream; the browser opens Live TV during a
    // deploy and /live-tv/channels times out. Marking the document resumed on
    // that silence would strand a running session for the life of the tab.
    const storage = memoryStorage(), wall = 1_000_000;
    storage.setItem("plurx_live_tv_hint_v1:stranded", String(wall - 600_000));
    const session = { session_id: "cap", live: true, channel: { id: "7.1" },
      playlist_url: "/api/v1/live-tv/sessions/cap/master.m3u8" };
    const run = shippedResume({
      storage, wall: () => wall,
      state: { lineupRead: false, protocols: null },
      answer: () => ({ outcome: "live", session }),
    });
    await run.resume(1, "#/live-tv");
    assert.deepEqual(run.asked, [], "nothing is asked before the lineup answers");
    assert.equal(run.state.resumed, false, "and the document has decided nothing yet");

    // The viewer navigates away and back; this time the lineup answers.
    run.state.lineupRead = true; run.state.protocols = [1, 2, 3];
    await run.resume(1, "#/live-tv");
    assert.deepEqual(run.asked, ["POST /live-tv/starts/stranded/resume"]);
    assert.equal(run.attached.length, 1, "the still-running session is rejoined");
    assert.equal(run.state.resumed, true);

    // A lineup that answers without the protocol settles it for good: nothing
    // to ask, and no reason to ask again.
    const legacy = shippedResume({
      storage: memoryStorage(), wall: () => wall,
      state: { protocols: null }, answer: () => ({ outcome: "live", session }),
    });
    await legacy.resume(1, "#/live-tv");
    assert.deepEqual(legacy.asked, []);
    assert.equal(legacy.state.resumed, true);
    // And the shipped view only claims a read that actually landed.
    const view = shipped("viewLiveTv");
    assert.ok(view.indexOf("LIVE_TV.lineupRead=true") > view.indexOf('await api("/live-tv/channels"'));
    assert.ok(view.indexOf("LIVE_TV.lineupRead=true") < view.indexOf("}catch(e){"),
      "the flag is set on the answered path, never in the failure path");
  });

  await test("a refused resume forgets the handle only when an owner decided it", async () => {
    // Web's documented behaviour, where Apple and Android differ: the resume
    // refusal goes through the same reducer a start refusal does.
    const refuse = (status, body) => () => {
      const error = new Error("refused");
      error.status = status; error.code = body.code;
      error.answer = { transport: "http", status, body };
      throw error;
    };
    const decided = shippedResume({
      storage: (() => { const s = memoryStorage(); s.setItem("plurx_live_tv_hint_v1:gone", "0"); return s; })(),
      answer: refuse(404, { code: "channel_not_found", retry: "never", owner_decided: true }),
    });
    await decided.resume(1, "#/live-tv");
    assert.deepEqual(decided.hints.list(), [],
      "an owner that decided the answer leaves nothing to retire");

    const undecided = shippedResume({
      storage: (() => { const s = memoryStorage(); s.setItem("plurx_live_tv_hint_v1:maybe", "0"); return s; })(),
      answer: refuse(503, { code: "owner_unavailable", retry: "now", owner_decided: false }),
    });
    await undecided.resume(1, "#/live-tv");
    assert.equal(undecided.hints.list().length, 1,
      "an answer no owner made leaves a start that may exist, and its only handle");
    assert.deepEqual(undecided.attached, []);
  });

  await test("a resumed session is watched on its own channel, playlist and heartbeat", async () => {
    // The fixture's session is a whole LiveTvActivated. A lineup that no
    // longer lists the channel — a stale read, a rescan — must not leave the
    // page watching something it cannot name.
    const session = START_CASES.resume.find(rule => rule.then === "reattach").body.session;
    const video = { paused: false, currentTime: 0, canPlayType: () => "maybe", play: async () => {} };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true, dataset: {} } };
    const state = { channels: [], serial: 1, selected: null, polling: false, lastFrames: 0 };
    let keepalives = 0, poll = null;
    const attach = new Function(
      "LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv", "API",
      `const location={hash:"#/live-tv"}, PAGE_RENDER_GENERATION=1, window={};
       function liveTvRefreshCaptionControls(){} function liveTvMessage(){} function liveTvFailure(){} function liveTvShowHost(){}
       function liveTvSetMode(){} function liveTvInPip(){ return false; }
       async function stopLiveTv(){} async function watchLiveTv(){}
       ${shipped("liveTvNow")}${shipped("liveTvAttachSession")} return liveTvAttachSession;`,
    )(
      state, { current: { session_id: session.session_id }, status: async () => ({ state: "active" }),
        keepalive: async () => { keepalives++; } },
      { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => 0 }, fn => { poll = fn; return 7; }, liveTv, "/api",
    );
    await attach(session, -1, 1, 1);
    assert.equal(state.selected, session.channel.id, "watching is the channel the owner named");
    assert.equal(video.src, `/api/live-tv/sessions/${session.session_id}/master.m3u8`,
      "the playlist is the capability's own same-origin route, not the wire URL");
    assert.ok(poll, "a resumed session is watched by the same tuner watchdog");
    video.currentTime = 30; await poll();
    assert.equal(keepalives, 1, "and renews the same lease a fresh start would");
  });

  await test("the negotiated protocol list decides the id and the recovery routes", async () => {
    // Cached per lineup read, from the channels response, exactly as shipped.
    assert.match(shell, /LIVE_TV\.protocols=Array\.isArray\(result\.protocols\)\?result\.protocols:null/);
    for (const rule of START_CASES.protocols) {
      const rows = [];
      const storage = memoryStorage();
      storage.setItem("plurx_live_tv_hint_v1:old", "0"); // an ancient orphan
      const lease = shippedLease({
        storage,
        protocols: Array.isArray(rule.channels_response.protocols) ? rule.channels_response.protocols : null,
        request: async (path, method, _t, _a, _k, body) => {
          rows.push({ path, method, body });
          return method === "POST" && path.endsWith("/sessions")
            ? { session_id: "cap", live: true } : { outcome: "retired" };
        },
      });
      await lease.LIVE_TV_LEASE.start("7.1");
      const post = rows.find(row => row.method === "POST" && row.path.endsWith("/sessions"));
      assert.equal("request_id" in post.body, rule.request_id, `${rule.case}: request_id on the body`);
      assert.equal(rows.some(row => row.path.startsWith("/live-tv/starts/")), rule.recovery_routes,
        `${rule.case}: recovery routes`);
      // A legacy ingress still leaves earlier hints alone for an upgrade.
      assert.ok(lease.LIVE_TV_HINTS.list().some(hint => hint.id === "old") !== rule.recovery_routes,
        `${rule.case}: an old hint is retired only where the routes exist`);
    }
  });

  await test("a dead but freshly touched hint is left alone, and the press still starts", async () => {
    const rows = [], storage = memoryStorage();
    const wall = 1_000_000;
    // Touched one keepalive ago: inside retire_orphan_after_keepalives × 5 s.
    storage.setItem("plurx_live_tv_hint_v1:fresh", String(wall - 5000));
    const lease = shippedLease({
      storage, wall: () => wall, channel: tabBus(),
      request: async (path, method, _t, _a, _k, body) => {
        rows.push({ path, method, body });
        return method === "POST" && path.endsWith("/sessions")
          ? { session_id: "cap", live: true } : { outcome: "retired" };
      },
    });
    await lease.LIVE_TV_LEASE.start("7.1");
    await settled(policy.liveContractTiming("retire_liveness_probe_ms") * 4);
    assert.equal(rows.filter(row => row.path.startsWith("/live-tv/starts/")).length, 0,
      "a hint younger than the keepalive window belongs to something alive");
    assert.ok(lease.LIVE_TV_HINTS.list().some(hint => hint.id === "fresh"));
    assert.ok(rows.some(row => row.method === "POST" && row.path.endsWith("/sessions")),
      "and the new start still reaches the server");
  });

  await test("a live sibling document claims its hint, and the press still starts", async () => {
    const rows = [], storage = memoryStorage();
    const wall = 1_000_000;
    // Old enough to look orphaned by the clock alone.
    storage.setItem("plurx_live_tv_hint_v1:sibling", String(wall - 600_000));
    const Channel = tabBus();
    const lease = shippedLease({
      storage, wall: () => wall, channel: Channel,
      request: async (path, method, _t, _a, _k, body) => {
        rows.push({ path, method, body });
        return method === "POST" && path.endsWith("/sessions")
          ? { session_id: "cap", live: true } : { outcome: "retired" };
      },
    });
    const watching = new Channel();
    watching.onmessage = event => {
      if (event.data && event.data.who === "sibling") watching.postMessage({ alive: "sibling" });
    };
    await lease.LIVE_TV_LEASE.start("7.1");
    await settled(policy.liveContractTiming("retire_liveness_probe_ms") * 4);
    assert.equal(rows.filter(row => row.path.startsWith("/live-tv/starts/")).length, 0,
      "a document that answered the probe keeps its session");
    assert.ok(lease.LIVE_TV_HINTS.list().some(hint => hint.id === "sibling"));
    assert.ok(rows.some(row => row.method === "POST" && row.path.endsWith("/sessions")),
      "a sibling's stream is never a reason to refuse this press");
  });

  await test("a document that is watching answers for its own hint, on the shipped channel", async () => {
    // Both halves of the probe are the shipped code here: one document holds a
    // live session, the other presses, and they only ever meet on the
    // BroadcastChannel. The watching document's hint is old enough on the wall
    // clock to look abandoned — being alive is what saves it.
    const Channel = tabBus(), storage = memoryStorage(), wall = 1_000_000;
    const served = (rows) => async (path, method, _t, _a, _k, body) => {
      rows.push({ path, method, body });
      return method === "POST" && path.endsWith("/sessions")
        ? { session_id: `cap-${rows.length}`, live: true } : { outcome: "retired" };
    };
    const rowsA = [], rowsB = [];
    const watching = shippedLease({ storage, wall: () => wall, channel: Channel, request: served(rowsA) });
    await watching.LIVE_TV_LEASE.start("7.1");
    const held = watching.LIVE_TV.hint;
    storage.setItem(`plurx_live_tv_hint_v1:${held}`, String(wall - 600_000));
    const pressing = shippedLease({ storage, wall: () => wall, channel: Channel, request: served(rowsB) });
    await pressing.LIVE_TV_LEASE.start("9.1");
    await settled(policy.liveContractTiming("retire_liveness_probe_ms") * 4);
    assert.equal(rowsB.filter(row => row.path.startsWith("/live-tv/starts/")).length, 0,
      "a live sibling's start is never retired out from under it");
    assert.ok(storage.getItem(`plurx_live_tv_hint_v1:${held}`) !== null);
    assert.ok(rowsB.some(row => row.method === "POST" && row.path.endsWith("/sessions")),
      "and the second document still gets its own tuner request to the owner");
  });

  await test("an old unclaimed hint is retired, and the press still starts", async () => {
    const rows = [], storage = memoryStorage();
    const wall = 1_000_000;
    storage.setItem("plurx_live_tv_hint_v1:orphan", String(wall - 600_000));
    const lease = shippedLease({
      storage, wall: () => wall, channel: tabBus(),
      request: async (path, method, _t, _a, _k, body) => {
        rows.push({ path, method, body });
        return method === "POST" && path.endsWith("/sessions")
          ? { session_id: "cap", live: true } : { outcome: "retired" };
      },
    });
    await lease.LIVE_TV_LEASE.start("7.1");
    // The press is already at the owner while the sweep is still inside its
    // liveness probe: dispatched, not awaited.
    assert.ok(rows.some(row => row.method === "POST" && row.path.endsWith("/sessions")),
      "and the press proceeds either way");
    assert.equal(rows.filter(row => row.path.startsWith("/live-tv/starts/")).length, 0,
      "the tuner request must not wait behind a quarter-second probe");
    await settled(policy.liveContractTiming("retire_liveness_probe_ms") * 4);
    assert.deepEqual(rows.filter(row => row.path.startsWith("/live-tv/starts/"))
      .map(row => `${row.method} ${row.path}`), ["DELETE /live-tv/starts/orphan"]);
    assert.ok(!lease.LIVE_TV_HINTS.list().some(hint => hint.id === "orphan"),
      "a typed 2xx retirement takes the handle away");
  });

  await test("the press does not wait for the liveness probe before reaching the owner", async () => {
    // §5.5's acceptance: a stored hint costs the viewer nothing. The probe
    // takes retire_liveness_probe_ms; the POST must already be gone by then,
    // and the order of the two requests on the wire is the proof.
    const rows = [], storage = memoryStorage(), wall = 1_000_000;
    storage.setItem("plurx_live_tv_hint_v1:orphan", String(wall - 600_000));
    const lease = shippedLease({
      storage, wall: () => wall, channel: tabBus(),
      request: async (path, method) => {
        rows.push(`${method} ${path}`);
        return method === "POST" && path.endsWith("/sessions")
          ? { session_id: "cap", live: true } : { outcome: "retired" };
      },
    });
    await lease.LIVE_TV_LEASE.start("7.1");
    assert.equal(rows[0], "POST /live-tv/channels/7.1/sessions",
      "the tuner request is the first thing on the wire, before any retire");
    await settled(policy.liveContractTiming("retire_liveness_probe_ms") * 4);
    assert.deepEqual(rows, ["POST /live-tv/channels/7.1/sessions", "DELETE /live-tv/starts/orphan"],
      "and the sweep lands beside it, not in front of it");
  });

  await test("a hint is a request id and a touch, and the browser may refuse to keep it", () => {
    let wall = 5000;
    const storage = memoryStorage();
    const hints = new liveTv.StartHints(() => storage, () => wall);
    hints.remember("a1");
    assert.deepEqual(hints.list(), [{ id: "a1", touchedAt: 5000 }]);
    assert.equal(storage.getItem("plurx_live_tv_hint_v1:a1"), "5000");
    wall = 9000; hints.touch("a1");
    assert.deepEqual(hints.list(), [{ id: "a1", touchedAt: 9000 }]);
    hints.forget("a1");
    assert.deepEqual(hints.list(), []);
    // Never a capability, never a token, never a channel or a tuner URL.
    hints.remember("b2");
    assert.deepEqual([...Array(storage.length).keys()].map(i => storage.key(i)),
      ["plurx_live_tv_hint_v1:b2"]);
    // A storage that throws costs the tidy-up and nothing else.
    const denied = new liveTv.StartHints(() => { throw new Error("storage denied"); }, () => 0);
    assert.deepEqual(denied.list(), []);
    denied.remember("c3"); denied.touch("c3"); denied.forget("c3");
  });

  await test("a browser that never confirms a DELETE does not collect hints forever", () => {
    let wall = 0;
    const storage = memoryStorage();
    const hints = new liveTv.StartHints(() => storage, () => wall);
    for (let i = 0; i < 40; i++) { wall = i; hints.remember(`id${i}`); }
    assert.equal(storage.length, 32, "the oldest handle is dropped, the new one is never refused");
    assert.ok(hints.list().some(hint => hint.id === "id39"));
  });

  await test("LAN HTTP request ids do not require the secure-context randomUUID API", () => {
    const id = new Function("crypto", `${shipped("liveTvRequestId")} return liveTvRequestId();`)({
      getRandomValues: bytes => { bytes.fill(42); return bytes; },
    });
    assert.equal(id, "2a".repeat(16));
    assert.match(id, /^[0-9a-f]{32}$/);
  });

  await test("an active session keeps its handle until the owner confirms the DELETE", async () => {
    const rows = [];
    const lease = shippedLease({
      request: async (path, method) => {
        rows.push(`${method} ${path}`);
        if (method === "POST" && path.endsWith("/sessions")) return { session_id: "cap", live: true };
        if (method === "DELETE" && path.startsWith("/live-tv/sessions/")) return null;
        return { outcome: "retired" };
      },
    });
    await lease.LIVE_TV_LEASE.start("7.1");
    assert.equal(lease.LIVE_TV_HINTS.list().length, 1, "a live session is still retirable");
    assert.equal(lease.LIVE_TV.hint, lease.LIVE_TV_HINTS.list()[0].id);
    await lease.LIVE_TV_LEASE.keepalive();
    await lease.LIVE_TV_LEASE.stop();
    assert.deepEqual(lease.LIVE_TV_HINTS.list(), [], "a confirmed release forgets the handle");
    assert.equal(lease.LIVE_TV.hint, null);
  });

  await test("a release the owner never confirmed keeps the handle for the next press", async () => {
    const lease = shippedLease({
      request: async (path, method) => {
        if (method === "POST" && path.endsWith("/sessions")) return { session_id: "cap", live: true };
        if (method === "DELETE" && path.startsWith("/live-tv/sessions/")) {
          const error = new Error("gateway");
          error.status = 502; error.answer = { transport: "http", status: 502, body: {} };
          throw error;
        }
        return { outcome: "retired" };
      },
    });
    await lease.LIVE_TV_LEASE.start("7.1");
    const held = lease.LIVE_TV.hint;
    await lease.LIVE_TV_LEASE.stop().catch(() => {});
    assert.ok(lease.LIVE_TV_HINTS.list().some(hint => hint.id === held),
      "an unconfirmed DELETE leaves the only handle that can retire it");
  });

  await test("both readiness views render every row the server sends, and gate nothing", () => {
    // The Developer enable sections are advisory by construction: they map the
    // server's `checks` array, whatever is in it. A row the daemon adds later
    // appears without a web change, and no row can disable a control.
    const guideView = new Function("esc", `${shipped("liveTvGuideReadyView")} return liveTvGuideReadyView;`)(
      value => String(value));
    const guide = guideView({
      source: "xmltv", guide_hours: 12, freshness: "fresh", age_seconds: 600,
      matched_channels: 7, lineup_channels: 9, programmes: 400, refresh_interval_seconds: 1200,
      checks: [
        { id: "guide_age", ready: true, message: "The guide is 10 minutes old; fetched at 03:04." },
        { id: "guide_next_refresh", ready: true, message: "The owner refreshes again in 9 minutes." },
        { id: "guide_persisted", ready: false, message: "No guide file on disk yet." },
        { id: "guide_last_error", ready: true, message: "The last refresh succeeded." },
      ],
    });
    for (const row of ["guide is 10 minutes old", "refreshes again in 9 minutes",
      "No guide file on disk yet", "last refresh succeeded"]) assert.ok(guide.includes(row), row);
    assert.equal((guide.match(/Met</g) || []).length, 3);
    assert.equal((guide.match(/Not met yet</g) || []).length, 1);
    assert.doesNotMatch(guide, /disabled/);
    assert.match(guide, /None of this blocks the switch/);

    const nodes = { ltreadiness: { innerHTML: "", textContent: "", isConnected: true } };
    const button = { disabled: false, isConnected: true };
    const check = new Function("api", "document", "esc", "settingsCurrent", "PAGE_RENDER_GENERATION",
      `${shipped("checkLiveTvReadiness")} return checkLiveTvReadiness;`)(
      async () => ({ ready: false, generation: 8, snapshot: null, checks: [
        { id: "device_reachable", ready: true, message: "The tuner answered on 10.42.4.100." },
        { id: "start_recovery", ready: false, message: "This owner does not retire or resume starts yet." },
      ] }),
      { getElementById: id => nodes[id] }, value => String(value), () => true, 1);
    return check(button).then(() => {
      assert.match(nodes.ltreadiness.innerHTML, /The tuner answered on 10\.42\.4\.100\./);
      assert.match(nodes.ltreadiness.innerHTML, /does not retire or resume starts yet/,
        "a row the server added must appear without a hand-written case for it");
      assert.match(nodes.ltreadiness.innerHTML, /Not ready:/);
      assert.match(nodes.ltreadiness.innerHTML, /advisory: enabling is not blocked/);
      assert.equal(button.disabled, false, "a red check never leaves the control disabled");
    });
  });

  await test("the guide polls on the owner's clock and stops on route change", async () => {
    // Every number the web guide loop paces on is the served contract table's,
    // and the served table is the fixture's. Seven keys, no local constant.
    for (const name of ["channel_coalesce_ms", "guide_poll_min_s", "guide_poll_after_next_refresh_s",
      "guide_poll_unavailable_s", "guide_poll_ceiling_s", "retire_liveness_probe_ms",
      "retire_orphan_after_keepalives", "start_replay_attempts"]) {
      assert.equal(policy.liveContractTiming(name), CONTRACT_TIMINGS[name],
        `${name} must reach the web reducer from the fixture`);
    }
    const timings = {
      guide_poll_min_s: CONTRACT_TIMINGS.guide_poll_min_s,
      guide_poll_after_next_refresh_s: CONTRACT_TIMINGS.guide_poll_after_next_refresh_s,
      guide_poll_unavailable_s: CONTRACT_TIMINGS.guide_poll_unavailable_s,
      guide_poll_ceiling_s: CONTRACT_TIMINGS.guide_poll_ceiling_s,
    };
    const now = 1_700_000_000;
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now + 60 }, now, timings), 65_000);
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now - 100 }, now, timings), 15_000,
      "the floor holds whatever the owner's next_refresh_at says");
    assert.equal(liveTv.guidePollDelayMs({ freshness: "unavailable" }, now, timings), 30_000);
    assert.equal(liveTv.guidePollDelayMs({ freshness: "fresh" }, now, timings), 15_000);
    // A read that threw is paced like an answer with nothing in it: same
    // information, and the three clients must not disagree about it.
    assert.equal(liveTv.guidePollDelayMs(null, now, timings), 30_000);
    // A far-future next_refresh_at must not park the grid until then.
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now + 86_400 }, now, timings), 1_200_000);
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now + 1_195 }, now, timings), 1_200_000,
      "the ceiling binds exactly where the owner's answer crosses it");
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now + 1_100 }, now, timings), 1_105_000,
      "and an answer inside the ceiling is still the owner's to give");
    // The floor outranks the ceiling if the two are ever set to cross: a
    // fan-out floor is a promise to the owner, a ceiling only to the viewer.
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now + 600 },
      now, { ...timings, guide_poll_ceiling_s: 5 }), 15_000);
    // A timings object without the key at all still answers.
    assert.equal(liveTv.guidePollDelayMs({ next_refresh_at: now + 86_400 }, now,
      { ...timings, guide_poll_ceiling_s: undefined }), 86_405_000);

    // And the shipped loop actually reschedules itself from the answer.
    const delays = [], answers = [
      { source: "xmltv", freshness: "fresh", channels: [], next_refresh_at: now + 60 },
      { source: "xmltv", freshness: "unavailable", channels: [] },
      { source: "xmltv", freshness: "fresh", channels: [], next_refresh_at: now + 604_800 },
    ];
    let reads = 0, pending = null;
    const load = new Function(
      "PlurxLiveTv", "PlaybackPolicy", "LIVE_TV", "api", "AbortSignal", "Date", "setTimeout", "clearTimeout",
      `let PAGE_RENDER_GENERATION=1; const location={hash:"#/live-tv"};
       function renderLiveTvChannels(){} function liveTvRefreshCaptionControls(){} function liveTvMessage(){}
       // The guide read also kicks the DVR wave off; that is its own test.
       async function loadLiveTvDvr(){}
       ${shipped("loadLiveTvGuide")}${shipped("scheduleLiveTvGuide")} return loadLiveTvGuide;`,
    )(
      liveTv, policy, { channels: [], guide: null, guideTimer: null },
      async () => { const answer = answers[reads++]; if (!answer) throw new Error("guide host down"); return answer; },
      { timeout: () => null }, { now: () => now * 1000 },
      (fn, ms) => { delays.push(ms); pending = fn; return delays.length; }, () => {},
    );
    await load(1, "#/live-tv");
    assert.deepEqual(delays, [65_000], "the owner said when it would have something new");
    await pending();
    assert.deepEqual(delays, [65_000, 30_000], "an unavailable guide asks again soon");
    await pending();
    assert.deepEqual(delays, [65_000, 30_000, 1_200_000],
      "an owner promising to refresh next week is still asked within the ceiling");
    await pending();
    assert.deepEqual(delays, [65_000, 30_000, 1_200_000, 30_000],
      "and a guide read that failed keeps asking, on the unavailable cadence");
    assert.equal(reads, 4, "the grid fills without the viewer leaving the page");
    assert.match(shipped("liveTvLeaveRoute"), /clearTimeout\(LIVE_TV\.guideTimer\)/);
    // Not one of those numbers is written into the page.
    assert.doesNotMatch(shipped("scheduleLiveTvGuide"), /\b(15|30|65)000\b/);
  });

  await test("Live TV awaits fullscreen exit before hiding, with iPhone entry fallback", async () => {
    const events = [], panel = { hidden: false, dataset: { mode: "full" }, contains: () => false };
    const video = { webkitEnterFullscreen: () => events.push("iphone-enter") };
    const exited = deferred(), neverSettles = deferred(), listeners = new Map();
    const document = { getElementById: id => id === "live-tv-host" ? panel : id === "live-tv-video" ? video : null,
      addEventListener: (name, listener) => listeners.set(name, listener),
      removeEventListener: (name, listener) => { if (listeners.get(name) === listener) listeners.delete(name); },
      fullscreenElement: panel, exitFullscreen: () => {
        events.push("exit"); exited.promise.then(() => {
          document.fullscreenElement = null; listeners.get("fullscreenchange")?.();
        });
        return neverSettles.promise;
      } };
    const state = { serial: 0 };
    const control = new Function("document", "location", "LIVE_TV", "LIVE_TV_LEASE", "detachLiveTvMedia",
      `${shipped("exitLiveTvPresentation")}${shipped("stopLiveTv")}${shipped("fullscreenLiveTv")}
       return {stopLiveTv,fullscreenLiveTv};`)(document, { hash: "#/live-tv" }, state, { stop: async () => events.push("release") }, () => events.push("detach"));
    const closing = control.stopLiveTv();
    await Promise.resolve();
    assert.deepEqual(events, ["exit", "release"]);
    assert.equal(panel.hidden, false, "the fullscreen subtree must remain mounted until exit settles");
    assert.equal(panel.dataset.mode, "slot", "a late fullscreenchange cannot reveal a stopped host");
    exited.resolve();
    await closing;
    assert.deepEqual(events, ["exit", "release", "detach"]);
    assert.equal(panel.hidden, true);
    control.fullscreenLiveTv();
    assert.equal(events.at(-1), "iphone-enter");
  });

  await test("Live TV retains an early release failure until presentation teardown", async () => {
    const exited = deferred(), panel = { hidden: false, contains: () => false };
    const listeners = new Map(), document = {
      getElementById: id => id === "live-tv-host" ? panel : null,
      addEventListener: (name, listener) => listeners.set(name, listener),
      removeEventListener: (name, listener) => { if (listeners.get(name) === listener) listeners.delete(name); },
      fullscreenElement: panel,
      exitFullscreen: () => exited.promise.then(() => {
        document.fullscreenElement = null; listeners.get("fullscreenchange")?.();
      }),
    };
    const control = new Function("document", "LIVE_TV", "LIVE_TV_LEASE", "detachLiveTvMedia",
      `${shipped("exitLiveTvPresentation")}${shipped("stopLiveTv")} return stopLiveTv;`)(
      document, { serial: 0 }, { stop: async () => { throw new Error("release failed"); } }, () => {});
    const closing = control();
    await Promise.resolve();
    assert.equal(panel.hidden, false);
    exited.resolve();
    await assert.rejects(closing, /release failed/);
    assert.equal(panel.hidden, true, "a release refusal must not leave the stopped presentation visible");
  });

  await test("theater viewers can discover Live TV without administrator access", () => {
    const nav = new Function("ME", "location", `${shipped("theaterNavItems")} return theaterNavItems;`)(
      { is_admin: false }, { hash: "#/live-tv" });
    const items = nav("live-tv");
    assert.deepEqual(items.find(item => item.tab === "live-tv"), {
      tab: "live-tv", href: "#/live-tv", label: "Live TV", on: true,
    });
    assert.equal(items.some(item => item.tab === "settings"), false);
  });

  await test("cluster tuner configuration stays editable while enabled", async () => {
    const writes = [], settings = { live_tv_enabled: true, live_tv_config_generation: 8 };
    const nodes = { ltip: { value: " 10.42.4.99 " },
      ltlimit: { value: "2" }, ltheight: { value: "720" } };
    const save = new Function("SETTINGS", "document", "liveTvSettingsWrite",
      `${shipped("saveLiveTvSettings")} return saveLiveTvSettings;`)(
      settings, { getElementById: id => nodes[id] }, async body => { writes.push(body); return true; });
    assert.equal(await save(), true);
    assert.deepEqual(writes, [{ live_tv_config_generation: 8,
      live_tv_device_ipv4: "10.42.4.99", live_tv_max_sessions: 2,
      live_tv_output_height: 720, live_tv_max_output_height: 720 }]);
  });

  await test("Developer enable saves even when readiness is unmet or unavailable", async () => {
    for (const readiness of ["Not met", "Readiness unavailable"]) {
      const writes = [], saved = { live_tv_enabled: true, live_tv_config_generation: 9 };
      const nodes = { "dev-live-tv-enable": { checked: true },
        "dev-live-tv-readiness": { textContent: readiness },
        "dev-live-tv-error": { textContent: "", isConnected: true } };
      let cached;
      const save = new Function("SETTINGS", "document", "api", "cacheSettings", "toast", "setCardSaved", "AbortSignal",
        `${shipped("saveLiveTvEnable")} return saveLiveTvEnable;`)(
        { live_tv_config_generation: 8 }, { getElementById: id => nodes[id] },
        async (route, request) => { writes.push([route, request.method, request.body]); return saved; },
        value => { cached = value; }, () => {}, button => { button.disabled = false; }, AbortSignal);
      const button = { disabled: false, isConnected: true };
      await save(button);
      assert.deepEqual(writes, [["/settings", "PUT", { live_tv_config_generation: 8, live_tv_enabled: true }]]);
      assert.equal(cached, saved);
      assert.equal(nodes["dev-live-tv-error"].textContent, "");
      assert.equal(button.disabled, false);
    }
  });

  // ---- the shared guide cases, reproduced by the web renderer -------------
  // tests/playback/live-tv-guide-cases.json is the same fixture the Apple and
  // Android suites read. Three reducers, one truth.
  const CASES = JSON.parse(
    fs.readFileSync(path.join(__dirname, "../playback/live-tv-guide-cases.json"), "utf8"),
  );

  await test("programmeAt answers every shared now/next/progress case", () => {
    for (const c of CASES.programme_at) {
      const answer = liveTv.programmeAt(CASES.guide, c.channel, c.now);
      assert.equal(
        answer.now ? answer.now.title : null,
        c.expect.now_title,
        `${c.name}: what is on now`,
      );
      assert.equal(
        answer.next ? answer.next.title : null,
        c.expect.next_title,
        `${c.name}: what is next`,
      );
      if (c.expect.progress === null) {
        assert.equal(answer.progress, null, `${c.name}: progress`);
      } else {
        assert.ok(
          Math.abs(answer.progress - c.expect.progress) < 1e-9,
          `${c.name}: progress ${answer.progress} != ${c.expect.progress}`,
        );
      }
    }
  });

  await test("an on-air cell reaches Record, and the handler has no airing branch", () => {
    // The defect this pins: the grid's click handler tuned the channel and
    // returned whenever the cell was airing, so the popover never opened and
    // Record and Record series were reachable only on a FUTURE programme.
    // "Record what I am watching" had no path on the web at all.
    const now = 1_000_000;
    const onAir = { title: "CBS Mornings", start: now - 600, end: now + 3000 };
    const acts = liveTv.dvrAiringActions(onAir, null, null, now);
    assert.equal(acts.watch, "now", "an airing programme can be tuned");
    assert.equal(acts.record, "record", "and recorded — this is the case that was unreachable");
    assert.equal(acts.series, true, "and a series rule can be made from it");
    // A reminder about something already started is about the past; the route
    // answers `airing_past`, so three verbs is the correct answer, not four.
    assert.equal(acts.remind, null, "no reminder for something already on");

    // Now the shipped handler itself. Compose it and RUN it for an airing
    // cell, rather than pattern-matching the source: the previous version of
    // this case asserted the old spelling (`if (airing)`) and a rewrite off
    // `cell.airing` with an `else` restored the defect while still passing.
    const handler = shell.slice(
      shell.indexOf("function liveTvGridCell("),
      shell.indexOf("function liveTvPopover("),
    );
    assert.ok(handler.includes("liveTvPopover("), "the handler opens the popover");
    const calls = [];
    const run = new Function(
      "liveTvGridLayout", "liveTvPopover", "liveTvSelect",
      `${handler}\nreturn liveTvGridCell;`,
    )(
      () => ({ rows: [{ channel: { id: "7.1" }, cells: [{ airing: true, programme: onAir }] }] }),
      (row, channelId) => calls.push(["popover", row && row.title, channelId]),
      id => calls.push(["select", id]),
    );
    run("7.1", 0);
    assert.deepEqual(
      calls, [["popover", "CBS Mornings", "7.1"]],
      "an airing cell opens its popover and does not short-circuit to tuning",
    );

    // And the handler must not consult `airing` at all: every cell reaches the
    // popover by the same path, whatever it is doing right now.
    assert.doesNotMatch(
      handler.replace(/\/\/[^\n]*/g, ""),
      /\bairing\b/,
      "no airing branch survives in the cell handler",
    );
  });

  await test("the cell popover is a modal that a keyboard can leave", () => {
    // Routing every cell through the popover is only an improvement if the
    // popover can be dismissed. It could not: no role, no focus, and the one
    // Escape listener did not know about it, so a keyboard user reached it by
    // tabbing past the whole page and then could not get out.
    const pop = shell.slice(
      shell.indexOf("function liveTvPopover("),
      shell.indexOf("function liveTvPaint("),
    );
    assert.match(pop, /setAttribute\("role","dialog"\)/, "it announces itself as a dialog");
    assert.match(pop, /setAttribute\("aria-modal","true"\)/, "and as modal");
    assert.match(pop, /\.focus\(\)/, "it takes the keyboard when it opens");
    assert.match(pop, /LIVE_TV_POP_OPENER/, "and hands it back to the cell on close");
    // Escape lives in the one capture listener that owns "close the topmost
    // modal", beside the connect dialog and the edit dialog — two Escape
    // handlers is how a key closes two things.
    const esc = shell.slice(
      shell.indexOf('window.addEventListener("keydown"'),
      shell.indexOf('// ---- global activity indicator'),
    );
    assert.match(esc, /liveTvPopoverOpen\(\)/, "Escape closes the cell popover");
    assert.match(esc, /liveTvPopover\(null\)/);
  });

  await test("no client's guide cell branches on airing — the defect was on all three", () => {
    // The web was not alone. Android branched the same way
    // (`if (cell.airing) onAiring(...) else onFuture(...)`), and Apple had
    // already fixed the phone behind `#if os(iOS)` while tvOS kept the old
    // branch — so the television, which is where the DVR acceptance starts,
    // was the one surface that could not record what was on.
    //
    // This reads the shipped client sources because neither native suite can
    // run here; it is a fence, not a substitute for their own tests.
    const read = rel => fs.readFileSync(path.join(__dirname, "../..", rel), "utf8");

    const androidCell = read("clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvGuideUi.kt");
    const cellFn = androidCell.slice(androidCell.indexOf("    cell: LiveTvGridCell,"));
    assert.match(cellFn, /onClick = \{ onFuture\(channel, cell\.programme\) \}/,
      "Android's grid cell opens the actions sheet unconditionally");
    assert.doesNotMatch(
      cellFn.slice(0, cellFn.indexOf("\n}")).replace(/\/\/[^\n]*/g, ""),
      /if \(cell\.airing\)/,
      "no airing branch in Android's grid cell",
    );

    const apple = read("clients/apple/Sources/LiveTvView.swift");
    assert.doesNotMatch(
      apple.replace(/\/\/[^\n]*/g, ""),
      /if cell\.airing \{ onAiring/,
      "no airing branch in Apple's guide cell, on either platform",
    );
    assert.doesNotMatch(
      apple, /#if os\(iOS\)\s*\n\s*onFuture\(row\.channel/,
      "and the sheet is not reached only on the phone",
    );
  });

  await test("gridLayout places every cell where the shared cases say, and never overlaps", () => {
    const g = CASES.grid;
    const layout = liveTv.gridLayout(
      CASES.guide, CASES.lineup, g.window, g.now, g.slot_seconds, g.px_per_slot,
    );
    assert.equal(layout.nowX, g.expect.now_line_x);
    assert.equal(layout.totalWidth, g.expect.total_width);
    assert.equal(layout.rows.length, CASES.lineup.length, "a channel never loses its row");
    for (const expected of g.expect.rows) {
      const row = layout.rows.find(r => r.channel.id === expected.channel);
      assert.ok(row, `no row for ${expected.channel}`);
      assert.deepEqual(
        row.cells.map(cell => ({
          title: cell.programme.title,
          left: cell.left,
          width: cell.width,
          airing: cell.airing,
          clipped: cell.clipped,
        })),
        expected.cells,
        `cells for ${expected.channel}`,
      );
      // Geometry the fixture cannot state row by row: cells advance and never
      // overlap, so a grid can never draw two programmes over one another.
      let edge = -Infinity;
      for (const cell of row.cells) {
        assert.ok(cell.left >= edge - 1e-9, `${expected.channel}: cells overlap`);
        assert.ok(cell.width > 0, `${expected.channel}: a zero-width cell`);
        edge = cell.left + cell.width;
      }
    }
    // 68.1 is in the lineup with no guide rows. The row is present and empty:
    // a channel must not disappear because the guide is thin.
    const protectedRow = layout.rows.find(r => r.channel.id === "68.1");
    assert.deepEqual(protectedRow.cells, []);
  });

  await test("the grid clips to its window and reports where the data ends", () => {
    const g = CASES.grid;
    const layout = liveTv.gridLayout(
      CASES.guide, CASES.lineup, g.window, g.now, g.slot_seconds, g.px_per_slot,
    );
    for (const row of layout.rows) {
      for (const cell of row.cells) {
        assert.ok(cell.left >= 0, "a cell starts before the window");
        assert.ok(cell.left + cell.width <= layout.totalWidth + 1e-9, "a cell runs past the window");
      }
    }
    assert.deepEqual(liveTv.gridSlots(g.window, g.slot_seconds).length, 4);
    // 11.1's last row ends inside the window, so the grid can say so.
    assert.equal(typeof liveTv.guideEnds(CASES.guide, "11.1", g.window), "number");
    // 7.1 runs past it, which is the ordinary case and gets no marker.
    assert.equal(liveTv.guideEnds(CASES.guide, "7.1", g.window), null);
  });

  await test("filterChannels answers every shared filtering case", () => {
    for (const c of CASES.filters) {
      const visible = liveTv.filterChannels(
        CASES.lineup,
        CASES.guide,
        { query: c.opts.query, filter: c.opts.filter, hideProtected: c.opts.hide_protected },
        CASES.grid.now,
      );
      assert.deepEqual(visible.map(channel => channel.id), c.expect, c.name);
    }
  });

  await test("adjacentChannel wraps in both directions and never lands nowhere", () => {
    for (const c of CASES.adjacent) {
      assert.equal(liveTv.adjacentChannel(c.visible, c.current, c.delta), c.expect, c.name);
    }
  });

  await test("Activity attributes a tuner to a channel, a viewer, a node and what is on", () => {
    // Anything on this server that uses a tuner and an encoder has to be
    // attributable from inside the product. The rows have always been in
    // /activity/detail; the page counted them in its summary and drew none of
    // them, so "2 active Live TV sessions" was the whole answer.
    const rows = new Function("esc",
      `${shipped("liveTvActivityRows")} return liveTvActivityRows;`)(value => String(value));
    assert.equal(rows(null, {}), "", "no sessions draws no section");
    assert.equal(rows([], {}), "");
    const html = rows([
      { channel_number: "7.1", channel_name: "WABC", user: "paul", owner_node_id: "node-b",
        encoder: "software", output_height: 720, age_seconds: 372, state: "active",
        programme_title: "City Beat" },
      { channel_number: "4.1", channel_name: "WNBC", user: "guest", owner_node_id: "node-b",
        encoder: "vaapi", output_height: 1080, age_seconds: 30, state: "starting" },
    ], { "node-b": "media1" });
    assert.match(html, /Live television/);
    assert.match(html, /City Beat/);
    assert.match(html, /media1/, "the roster name, not the raw node id");
    assert.doesNotMatch(html, /node-b/, "a node with a known name never shows its id");
    assert.match(html, /6 min · active/);
    // A row the guide cannot describe is still a row: the session is real
    // whether or not anything knows what is on it.
    assert.match(html, /WNBC/);
    assert.match(html, /—/);
  });

  await test("a guide that is off, empty or erroring is a rendered state, not a failure", () => {
    const empty = { source: "off", freshness: "unavailable", channels: [] };
    assert.deepEqual(liveTv.programmeAt(empty, "7.1", CASES.grid.now), {
      now: null, next: null, progress: null,
    });
    assert.deepEqual(liveTv.programmeAt(null, "7.1", 0), { now: null, next: null, progress: null });
    // Every channel still renders, with a row and no cells.
    const layout = liveTv.gridLayout(empty, CASES.lineup, CASES.grid.window, CASES.grid.now, 1800, 240);
    assert.equal(layout.rows.length, CASES.lineup.length);
    assert.ok(layout.rows.every(row => row.cells.length === 0));
    // And filtering still works without a guide behind it.
    assert.deepEqual(
      liveTv.filterChannels(CASES.lineup, empty, { query: "wabc" }, 0).map(c => c.id),
      ["7.1"],
    );
    // The guide has its own error copy, or every guide failure would read
    // "Tuner owner unavailable".
    const view = liveTv.errorView({ code: "guide_unavailable" });
    assert.equal(view.title, "No programme guide yet");
    assert.match(view.detail, /Channels still play/);
  });

  // ---- recording ----------------------------------------------------------

  const NOW = 1_750_000_000;
  const airing = (over) => Object.assign({
    id: "row-1", channel_id: "7.1", airing_start: NOW + 3600, airing_end: NOW + 7200,
    capture_start: NOW + 3540, capture_end: NOW + 7320, state: "scheduled", rule_id: null,
    guide_number: "7.1", channel_name: "WABC", title: "Jeopardy!", bytes: 0,
  }, over || {});

  await test("a schedule row picks exactly one mark, and a reminder adds the second", () => {
    const marks = (row, reminder) => liveTv.dvrMarks(row, reminder || null, NOW).map(m => [m.kind, m.shape]);
    assert.deepEqual(marks(airing()), [["once", "dot"]]);
    assert.deepEqual(marks(airing({ rule_id: "rule-9" })), [["series", "dots"]]);
    assert.deepEqual(marks(airing({ state: "conflict" })), [["conflict", "dot"]]);
    assert.deepEqual(marks(airing({ state: "withdrawn" })), [["withdrawn", "hollow"]]);
    assert.deepEqual(marks(airing({ state: "stale" })), [["stale", "hollow"]]);
    assert.deepEqual(marks(airing({ state: "recording" })), [["recording", "rec"]]);
    // `?cancelled=1` keeps a skipped row in the schedule so it can be
    // restored. It is not scheduled, and it must not look scheduled.
    assert.deepEqual(marks(airing({ state: "cancelled" })), []);
    // An airing nothing is recording still earns a bell when somebody asked
    // to be told about it.
    assert.deepEqual(marks(null, { id: "r" }), [["reminder", "bell"]]);
    assert.deepEqual(marks(airing({ state: "recording" }), { id: "r" }),
      [["recording", "rec"], ["reminder", "bell"]]);
    // The underline measures the padded capture, not the programme: it has to
    // reach its end when the file closes.
    const half = liveTv.dvrMarks(airing({ state: "recording" }), null, NOW + 3540 + 1890)[0];
    assert.equal(Math.round(half.progress * 100), 50);
    // Clamped at both ends: a late read of a closed capture is full, not 103%.
    assert.equal(liveTv.dvrCaptureProgress(airing({ capture_start: NOW - 100, capture_end: NOW }), NOW + 99), 1);
    assert.equal(liveTv.dvrCaptureProgress(airing({ capture_start: NOW + 10 }), NOW), 0);
    assert.equal(liveTv.dvrCaptureProgress(null, NOW), 0);

    // The index is keyed by (channel, start), which is what an airing IS —
    // never by the row id, which a rule's expansion may not have yet.
    const index = liveTv.dvrIndex({ rows: [airing()] },
      [{ channel_id: "7.1", airing_start: NOW + 3600, state: "armed", id: "r1" },
       { channel_id: "7.1", airing_start: NOW + 9999, state: "acked", id: "r2" }]);
    assert.equal(index.row("7.1", NOW + 3600).id, "row-1");
    assert.equal(index.row("7.1", NOW + 1), null, "a start that is not an airing is not one");
    assert.equal(index.row("9.1", NOW + 3600), null, "another channel at the same instant is another airing");
    assert.equal(index.reminder("7.1", NOW + 3600).id, "r1");
    // `acked` is history: a reminder somebody has already dismissed must not
    // keep a bell on the cell.
    assert.equal(index.reminder("7.1", NOW + 9999), null);
  });

  await test("a cell offers only the verbs the routes would accept", () => {
    const programme = { start: NOW + 3600, end: NOW + 7200, title: "Jeopardy!" };
    const future = liveTv.dvrAiringActions(programme, null, null, NOW);
    assert.deepEqual(future, { watch: "at", record: "record", series: true, remind: "set" });
    // On air: Watch tunes, and a reminder about a programme that has started
    // is a reminder about the past — the route answers `airing_past`.
    const onAir = liveTv.dvrAiringActions(programme, null, null, NOW + 3601);
    assert.equal(onAir.watch, "now");
    assert.equal(onAir.remind, null);
    // Under a minute left is not worth a tuner, a file and a library item.
    const done = liveTv.dvrAiringActions(programme, null, null, NOW + 7141);
    assert.deepEqual(done, { watch: null, record: null, series: false, remind: null });
    // An airing already on the schedule is skipped, not recorded twice.
    assert.equal(liveTv.dvrAiringActions(programme, airing(), null, NOW).record, "skip");
    assert.equal(liveTv.dvrAiringActions(programme, airing({ state: "conflict" }), null, NOW).record, "skip");
    assert.equal(liveTv.dvrAiringActions(programme, airing({ state: "recording" }), null, NOW).record, "stop");
    assert.equal(liveTv.dvrAiringActions(programme, null, { id: "r" }, NOW).remind, "clear");
    // A malformed guide row is total here, exactly as programmeAt is.
    assert.deepEqual(liveTv.dvrAiringActions({}, null, null, NOW),
      { watch: null, record: null, series: false, remind: null });
  });

  await test("the tuner line counts the schedule, not the status snapshot", () => {
    const status = { slots: { max: 4, reserve: 1, recording: 0 } };
    const schedule = { rows: [airing({ state: "recording" }), airing({ state: "recording", id: "b" }),
      airing({ state: "scheduled", id: "c" })] };
    assert.equal(liveTv.dvrTunerLine(status, schedule), "2 of 4 tuners · 1 reserved for viewing");
    // With no schedule yet the status's own count is the only answer there is.
    assert.equal(liveTv.dvrTunerLine({ slots: { max: 4, reserve: 1, recording: 3 } }, null),
      "3 of 4 tuners · 1 reserved for viewing");
    assert.equal(liveTv.dvrTunerLine({ slots: { max: 1, reserve: 0, recording: 0 } }, null), "0 of 1 tuner");
    // Nothing to say about a server that has not answered.
    assert.equal(liveTv.dvrTunerLine(null, null), "");
    assert.equal(liveTv.dvrTunerLine({ slots: { max: 0, reserve: 1, recording: 0 } }, null), "");
  });

  await test("the countdown is coarse above a minute and retires itself at the start", () => {
    assert.equal(liveTv.dvrCountdown(0), "now");
    assert.equal(liveTv.dvrCountdown(-30), "now");
    assert.equal(liveTv.dvrCountdown(45), "in 45s");
    assert.equal(liveTv.dvrCountdown(60), "in 1 min");
    assert.equal(liveTv.dvrCountdown(3599), "in 59 min");
    assert.equal(liveTv.dvrCountdown(3600), "in 1h");
    assert.equal(liveTv.dvrCountdown(7500), "in 2h 5m");

    const reminder = { airing_start: NOW + 300, lead_s: 300 };
    const fired = liveTv.dvrReminderCountdown(reminder, NOW);
    assert.equal(fired.fraction, 1);
    assert.equal(fired.label, "in 5 min");
    assert.equal(fired.expired, false);
    const half = liveTv.dvrReminderCountdown(reminder, NOW + 150);
    assert.equal(half.fraction, 0.5);
    const over = liveTv.dvrReminderCountdown(reminder, NOW + 300);
    assert.equal(over.expired, true);
    assert.equal(over.fraction, 0);
    // "Watch at 8:00" is a reminder with no lead at all, and a zero lead must
    // not divide the bar by zero.
    assert.equal(liveTv.dvrReminderCountdown({ airing_start: NOW + 10, lead_s: 0 }, NOW).fraction, 1);
  });

  await test("the marks fit the cell that already exists, and the overlay reads no keys", () => {
    // The "Live only" hint is gone, and the four verbs stand where it was.
    assert.doesNotMatch(shell, /Live only — plurx does not record/);
    const pop = shipped("liveTvPopoverActions");
    for (const verb of ["Watch", "Record", "Record series", "Remind me"])
      assert.ok(pop.includes(`>${verb}<`) || pop.includes(`${verb} `), `the popover offers ${verb}`);
    assert.match(pop, /Watch at \$\{esc\(liveTvClock\(programme\.start\)\)\}/,
      "a future cell's Watch names the time it will start");
    assert.match(pop, /liveTvRemind\(\$\{esc\(at\)\},\$\{start\},0\)/,
      "and sets a reminder with no lead, which is what “watch it then” means");
    assert.match(pop, /liveTvTunerLine\(\)/);

    // Eight pixels, absolutely placed: the grid's row height and the cell box
    // are the numbers the whole page is laid out on, and a mark may not move
    // either of them.
    assert.match(shell, /\.lt-mark\{[^}]*position:absolute/s);
    assert.match(shell, /\.lt-mark \.dot\{[^}]*width:8px;height:8px/s);
    assert.match(shell, /\.lt-recbar\{[^}]*position:absolute[^}]*bottom:0/s);
    assert.match(shell, /\.lt-grow\{[^}]*height:52px/s, "the grid row height is unchanged");
    assert.match(shell, /\.lt-row\{[^}]*min-height:78px/s, "the list row height is unchanged");
    assert.match(shipped("liveTvGridMarkup"), /liveTvMarks\(row\.channel\.id,cell\.programme,false\)/);
    assert.match(shipped("liveTvRowMarkup"), /liveTvMarks\(channel\.id,at\.now,true\)/);

    // The overlay is buttons the whole way down. `scripts/player-input-fence`
    // enforces this across the tree; asserting it here says why it is true.
    const overlay = shipped("paintDvrReminder");
    assert.doesNotMatch(overlay, /addEventListener/);
    assert.doesNotMatch(overlay, /onkey/i);
    for (const verb of ["Watch", "Record", "Dismiss"])
      assert.ok(overlay.includes(`>${verb}<`), `the overlay offers ${verb}`);
    assert.match(overlay, /dvrReminderDismiss\(/);
    assert.match(shipped("dvrReminderDismiss"), /dvrReminderAck/);
    assert.match(shipped("dvrReminderAck"), /\/ack/, "Dismiss acks, so a second device does not show it again");
    assert.match(shell, /\.dvr-remind\{[^}]*left:16px;bottom:16px/s, "lower-left");
    // One global 30 s timer beside the activity one, never a per-page poller.
    assert.match(shell, /if\(!DVR_REMINDER_TIMER\) DVR_REMINDER_TIMER=setInterval\(pollDvrReminders,30000\);/);
    assert.match(shipped("pollDvrReminders"), /api\("\/dvr\/reminders\?due=1"\)/);

    // Live TV viewer rows keep having no Stop, and the transcode/VOD verb is
    // still the transcode/VOD verb.
    assert.doesNotMatch(shipped("liveTvActivityRows"), /Stop/);
    assert.match(shipped("stopSession"), /\/activity\/sessions\//);
    assert.doesNotMatch(shipped("stopSession"), /dvr/);
    assert.match(shipped("stopDvrRecording"), /liveTvStopRecording/);
    assert.match(shipped("liveTvStopRecording"), /\/dvr\/recordings\//);
    assert.doesNotMatch(shipped("stopDvrRecording"), /setTimeout/,
      "Stop uses the shared Activity refresh rather than creating a second poller");
  });

  await test("a running capture describes itself for the row that can stop it", () => {
    assert.equal(
      liveTv.dvrRecordingDetail(airing({ state: "recording", capture_end: NOW + 720, bytes: 1_430_000_000 }), NOW),
      "7.1 WABC · 12 min left · 1.4 GB");
    // A capture that has written nothing yet says nothing about bytes rather
    // than claiming 0.0 GB, and a finished one never counts backwards.
    assert.equal(liveTv.dvrRecordingDetail(airing({ capture_end: NOW - 600 }), NOW), "7.1 WABC · 0 min left");
    assert.equal(liveTv.dvrRecordingDetail(null, NOW), "");
  });

  await test("the live envelope claims the audio output's channels for AAC, not a fixed stereo", () => {
    const ceiling = new Function(`${shipped("liveTvAacChannelCeiling")}; return liveTvAacChannelCeiling;`)();
    assert.equal(ceiling(undefined), 2);
    assert.equal(ceiling(1), 2);
    assert.equal(ceiling(2), 2);
    assert.equal(ceiling(6), 6);
    assert.equal(ceiling(8), 6);
    const envelope = shipped("liveTvPlaybackEnvelope");
    assert.match(envelope, /max_channels:codec==="aac"\?aacChannels:8/);
    assert.doesNotMatch(envelope, /max_channels:codec==="aac"\?2:8/);
  });
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
