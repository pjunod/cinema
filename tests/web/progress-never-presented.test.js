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

function harness(video,player){
  const posts=[];
  const p=Object.assign({fileId:7,offset:0},player);
  const c=vm.createContext({
    PLAYER:p, ITEM_FOR_FILE:{7:'42'},
    playbackOwnsAttachedMedia:()=>true,
    document:{getElementById:()=>video},
    api:async(url,req)=>{posts.push({url,...req});},
    Math,
  });
  vm.runInContext(source('reportProgress'),c);
  return {c,posts,p};
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
