"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const path = require("node:path");

const web = path.join(__dirname, "../../crates/plurxd/src/web/player");
const transport = fs.readFileSync(path.join(web, "transport.js"), "utf8");
const diagnosis = fs.readFileSync(path.join(web, "stall-diagnosis.js"), "utf8");

function shippedFunction(source, name) {
  const start = source.search(new RegExp(`\\n(?:async )?function ${name}\\(`));
  assert.ok(start >= 0, `${name} must remain in its shipped source`);
  const body = source.slice(start + 1);
  const next = body.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? body : body.slice(0, next + 1);
}

function makeBoundary({ duringAsk = () => {}, ask = async () => ({ id: "candidate-2" }) } = {}) {
  const incumbent = { id: "candidate-1", route: "encode", target_height: 1080 };
  const candidate = { id: "candidate-2", route: "encode", target_height: 720 };
  const player = { controlSeek: { sequence: 4 }, mediaAttachment: 9, wantsPlayback: true,
    qualityCandidateId: incumbent.id, qualityCandidates: [incumbent], abr: { switching: false } };
  const video = {};
  const calls = { switches: [], seeks: [], begins: [] };
  const scope = {
    PLAYER: player,
    document: { getElementById: id => id === "video" ? video : null },
    qualityForce: () => "auto",
    naturalBoundaryQualityCandidate: async (...args) => {
      const result = await ask(...args);
      duringAsk(scope, player);
      return result ? candidate : null;
    },
    hasPendingPlaybackOpen: () => false,
    switchAutoCandidate: (...args) => { calls.switches.push(args); return Promise.resolve(true); },
    seekTo: (...args) => calls.seeks.push(args),
    beginPlaybackControlSeek: (...args) => calls.begins.push(args),
  };
  const invoke = new Function("scope", `with(scope){${shippedFunction(transport, "resumeQualityBoundary")}\nreturn p=>resumeQualityBoundary(p);}`)(scope);
  return { player, video, candidate, incumbent, calls, scope, invoke };
}

test("resume quality boundary delegates an optional upgrade to the retained incumbent owner", async () => {
  const h = makeBoundary();
  assert.equal(await h.invoke(h.player), true);
  assert.equal(h.calls.switches.length, 1);
  assert.deepEqual(h.calls.switches[0], [h.player, h.video, h.incumbent,
    { candidate: h.candidate, reason: "resume_quality_boundary", transition: "upgrade" }]);
  assert.deepEqual(h.calls.seeks, []);
  assert.deepEqual(h.calls.begins, []);
});

test("resume quality boundary stands down when a newer transport intent wins the async ask", async () => {
  const supersede = [
    (_scope, p) => { p.controlSeek = { sequence: 5 }; },
    (_scope, p) => { p.wantsPlayback = false; },
    (_scope, p) => { p.mediaAttachment += 1; },
    scope => { scope.PLAYER = {}; },
    (_scope, p) => { p.qualityCandidateId = "candidate-2"; },
  ];
  for (const duringAsk of supersede) {
    const h = makeBoundary({ duringAsk });
    assert.equal(await h.invoke(h.player), false);
    assert.deepEqual(h.calls.switches, []);
    assert.deepEqual(h.calls.seeks, []);
  }
});

test("the real auto candidate owner retains the incumbent when preparation is unavailable", async () => {
  let released = 0, reopened = 0, began = 0;
  const player = { abr: { switching: false }, autoFallbackInFlight: false,
    qualityCandidates: [], controlSeek: { sequence: 1 }, mediaAttachment: 2 };
  const current = { id: "incumbent", route: "encode", target_height: 1080 };
  const candidate = { id: "optional", route: "encode", target_height: 720 };
  const scope = {
    PLAYER: player,
    claimAutoFallback: () => true,
    playbackControlSelection: () => ({ quality: "auto" }),
    preparedHandoffOffered: () => false,
    directedChangeIncumbentReady: () => true,
    releaseAutoFallback: () => { released += 1; },
    beginPlaybackControlSeek: () => { began += 1; },
    requestPlaybackMediaChange: () => { reopened += 1; },
    positionForPlaybackIntent: () => 42,
  };
  const invoke = new Function("scope", `with(scope){${shippedFunction(diagnosis, "switchAutoCandidate")}\nreturn (...args)=>switchAutoCandidate(...args);}`)(scope);
  assert.equal(await invoke(player, {}, current,
    { candidate, reason: "resume_quality_boundary", transition: "upgrade" }), false);
  assert.equal(player.abr.switching, false);
  assert.equal(released, 1);
  assert.equal(reopened, 0);
  assert.equal(began, 0);
});
