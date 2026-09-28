'use strict';
// A failed startup must not report position 0 over a saved resume point.
//
// `reportProgress` builds its beat from `bookOffset + (offset + currentTime)`.
// When a session never publishes a playlist the element stays at readyState 0
// with currentTime 0, and on a VOD or direct timeline the offset is 0 too — so
// the beat that reaches `POST /items/:id/progress` is literally position 0.
// The server takes it and the resume point is gone. Measured on a production
// node on 2026-09-22: one unplayable 26-second startup fires five or six of
// these, and `Cactus Pears` went from 33:17 to five seconds exactly that way.
//
// The guard is "a zero beat needs a witness, a positioned beat does not", and
// the witness is scoped to the CURRENT attachment — a retry or a quality reopen
// reuses the player object while resetting both the element and `offset`.
const {test}=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm');
const {shellSource}=require('./shell-source.js');
const html=shellSource().bodyScript;
function source(name){let at=html.indexOf(`async function ${name}(`);if(at<0)at=html.indexOf(`function ${name}(`);assert.ok(at>=0,`no ${name} in the served shell`);return html.slice(at,html.indexOf('\n}',at)+2);}

// A player object as the shipped code builds one: an attachment token minted
// per attach, and whatever offset the route gave it.
function attach(p){ p.mediaAttachment={id:(p._mediaAttachmentOrdinal||0)+1}; p._mediaAttachmentOrdinal=p.mediaAttachment.id; return p; }

// A clock the test drives, because the paused-repeat guard below is written in
// terms of elapsed time and a real one would make it a race.
function harness(video,player,options={}){
  const posts=[];
  const p=Object.assign({fileId:7,offset:0},player);
  const clock={ms:0};
  const c=vm.createContext({
    PLAYER:p, ITEM_FOR_FILE:{7:'42'},
    playbackOwnsAttachedMedia:()=>true,
    document:{getElementById:()=>video},
    api:options.api||(async(url,req)=>{posts.push({url,...req});}),
    performance:{now:()=>clock.ms},
    Math,
  });
  vm.runInContext(shippedConst('PAUSED_BEAT_FLOOR_MS'),c);
  vm.runInContext(source('reportProgress'),c);
  // A `const` run in a context is lexical, not a property of it, so the value
  // is read back as an expression rather than off `c`.
  const floorMs=vm.runInContext('PAUSED_BEAT_FLOOR_MS',c);
  assert.equal(typeof floorMs,'number');
  return {c,posts,p,clock,floorMs};
}
// The floor is a shipped constant, so the test reads it rather than repeating
// the number: a change to it that broke a reader would otherwise pass here.
function shippedConst(name){
  const match=html.match(new RegExp(`\\nconst ${name}=[^;]*;`));
  assert.ok(match,`the shell no longer declares const ${name}`);
  return match[0];
}

test('a startup that never presented a frame reports nothing',async()=>{
  const {c,posts,p}=harness({readyState:0,currentTime:0}); attach(p);
  await c.reportProgress(7,false);
  await c.reportProgress(7,false);
  assert.deepEqual(posts,[],'a session with no timeline has no position to report');
});

test('a loaded player reports the position it is actually at',async()=>{
  const {c,posts,p}=harness({readyState:4,currentTime:1832},{knownDur:3125131}); attach(p);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1);
  assert.equal(posts[0].url,'/items/42/progress');
  assert.equal(posts[0].body.position_ms,1832000);
});

// The server's `plurx_watched_seconds_total{method}` is labelled by what the
// beat names. Without it every web second is `unknown`, and bytes per watched
// minute cannot be split by delivery method.
test('a beat names the delivery method it is playing through',async()=>{
  const {c,posts,p}=harness({readyState:4,currentTime:600},{knownDur:3125131,method:'remux'}); attach(p);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1);
  assert.equal(posts[0].body.method,'remux');
});

// HAVE_METADATA is the boundary on purpose: a player paused at metadata with a
// real offset behind it is reporting a real position. Tightening the predicate
// would silently suppress it, so pin it.
test('metadata alone is enough of a witness',async()=>{
  const {c,posts,p}=harness({readyState:1,currentTime:0},{knownDur:3125131}); attach(p);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1,'readyState 1 is a timeline; the beat is real');
  assert.equal(posts[0].body.position_ms,0);
});

// closePlayer reports on behalf of a predecessor player through `attachedOwner`,
// against an element it may no longer own. That beat carries the predecessor's
// own playhead in `offset`, and it must survive: it is the only save of a title
// the viewer watched for ninety minutes.
test("a predecessor's close-time beat still reports its playhead",async()=>{
  const {c,posts,p}=harness({},{}); // no readyState at all: the element is gone
  const predecessor={fileId:7,offset:5400,knownDur:7_200_000};
  await c.reportProgress(7,false,predecessor);
  assert.equal(posts.length,1,'a positioned beat needs no witness');
  assert.equal(posts[0].body.position_ms,5_400_000);
  void p;
});

// The bug one button press later: Try again after a stall reuses the player,
// resets the element and zeroes `offset`. Evidence from the attachment that
// played must not license the zero beats of the attachment that did not.
test('evidence does not carry across a re-attach',async()=>{
  const video={readyState:4,currentTime:1997};
  const {c,posts,p}=harness(video,{offset:0,knownDur:6_831_648}); attach(p);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1);
  assert.equal(posts[0].body.position_ms,1_997_000);
  // stall → Try again: same player object, new attachment, element back to zero
  attach(p); p.offset=0; video.readyState=0; video.currentTime=0;
  await c.reportProgress(7,false);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1,'a retry that never loads must not erase what the first attachment earned');
});

// Start over is a real zero, and the element proves it.
test('a genuine start-over reports its zero',async()=>{
  const {c,posts,p}=harness({readyState:4,currentTime:0},{knownDur:6_831_648}); attach(p);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1);
  assert.equal(posts[0].body.position_ms,0);
});

// ---------------------------------------------------------------------------
// F-web-12: the paused repeat. A player left paused beat every five seconds
// forever, reporting one position nobody had moved. What is dropped is the
// repeat, and only where no reader of the route needs the cadence — which is
// why direct play is exempt and why the repeats still go out once a minute.
// (The plan named player-dom.test.js for these; they are here instead, because
// the harness that drives the shipped `reportProgress` is this one.)

const pausedElement=()=>({readyState:4,currentTime:900,paused:true});

test('a paused remux repeats its position at most once a floor',async()=>{
  const {c,posts,p,clock,floorMs}=harness(pausedElement(),{method:'transcode',knownDur:7_200_000});
  attach(p);
  await c.reportProgress(7,false);
  assert.equal(posts.length,1,'the first paused beat is the one that carries the position');
  clock.ms+=5_000; await c.reportProgress(7,false);
  clock.ms+=5_000; await c.reportProgress(7,false);
  assert.equal(posts.length,1,'a paused repeat at an unchanged position was posted again');
  // Trakt removes a session quiet for 150 s and can never scrobble its stop
  // afterwards, so the repeat is delayed, not abolished.
  clock.ms+=floorMs; await c.reportProgress(7,false);
  assert.equal(posts.length,2,'a paused player went silent past the floor every reader depends on');
  assert.ok(floorMs<150_000,'the floor must stay under Trakt IDLE_PAUSE (crates/plurxd/src/trakt.rs)');
});

test('a paused DIRECT PLAY keeps every beat',async()=>{
  // `DirectPlays` in crates/plurxd/src/delivery.rs prunes at IDLE_TIMEOUT
  // (30 s) and this beat is its only signal: a viewer paused with the film
  // buffered issues no further range requests.
  const {c,posts,p,clock}=harness(pausedElement(),{method:'direct_play',knownDur:7_200_000});
  attach(p);
  await c.reportProgress(7,false);
  clock.ms+=5_000; await c.reportProgress(7,false);
  clock.ms+=5_000; await c.reportProgress(7,false);
  assert.equal(posts.length,3,'suppressing a direct play would drop it off the Activity page');
});

test('a paused player that moves reports the move',async()=>{
  const video={readyState:4,currentTime:900,paused:true};
  const {c,posts,p,clock}=harness(video,{method:'transcode',knownDur:7_200_000});
  attach(p);
  await c.reportProgress(7,false);
  clock.ms+=5_000; video.currentTime=1200;   // paused, then scrubbed
  await c.reportProgress(7,false);
  assert.equal(posts.length,2);
  assert.equal(posts[1].body.position_ms,1_200_000,'a seek while paused is a new position, not a repeat');
});

test('a playing player is never suppressed',async()=>{
  const {c,posts,p,clock}=harness({readyState:4,currentTime:900,paused:false},
    {method:'transcode',knownDur:7_200_000});
  attach(p);
  await c.reportProgress(7,false);
  clock.ms+=5_000; await c.reportProgress(7,false);
  assert.equal(posts.length,2,'an unchanged position while PLAYING is a stall, which the server still wants to see');
});

test('a beat the server never received does not suppress the next one',async()=>{
  // The close beat is the resume point. If a failed post counted as delivered,
  // a close right after it would be dropped and the position lost for good.
  let fail=true;
  const posts=[];
  const {c,p}=harness({readyState:4,currentTime:900,paused:true},
    {method:'transcode',knownDur:7_200_000},
    {api:async(url,req)=>{ if(fail) throw new Error('offline'); posts.push({url,...req}); }});
  attach(p);
  await c.reportProgress(7,false);
  assert.deepEqual(posts,[],'the fixture refused the first beat');
  fail=false;
  await c.reportProgress(7,false);
  assert.equal(posts.length,1,'a failed beat suppressed the one that would have replaced it');
  assert.equal(posts[0].body.position_ms,900_000);
});
