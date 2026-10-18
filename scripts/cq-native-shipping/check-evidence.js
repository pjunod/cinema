#!/usr/bin/env node
// Refuse to call a native campaign a pass unless the shaping proxy actually
// carried the media: every stage the UI test requested — and stage 0, which
// every playing phase starts in — must show media bytes delivered through the
// proxy. A run whose app rediscovered the real server, or whose shaper never
// saw a segment, exits non-zero here whatever xcodebuild reported.
//   check-evidence.js EVIDENCE_JSON TEST_LOG PHASE
"use strict";
const fs = require("node:fs");

function requestedStages(log, phase) {
  const stages = new Set(["campaign", "auto", "probe"].includes(phase) ? [0] : []);
  for (const line of log.split("\n")) {
    const m = /CQEVT (\{.*\})/.exec(line);
    if (!m) continue;
    let e;
    try { e = JSON.parse(m[1]); } catch { continue; }
    if (e.kind === "link_request" && Number.isInteger(e.stage)) stages.add(e.stage);
  }
  return [...stages].sort((a, b) => a - b);
}

function verify(evidence, log, phase) {
  const problems = [];
  const stages = Array.isArray(evidence?.shaping?.stages) ? evidence.shaping.stages : [];
  const total = stages.reduce((sum, stage) => sum + (Number(stage.bytes) || 0), 0);
  if (!(total > 0)) problems.push("the proxy delivered no bytes at all: the app did not go through it");
  for (const index of requestedStages(log, phase)) {
    const stage = stages[index];
    if (!stage) { problems.push(`stage ${index} was requested but the shaper has no such stage`); continue; }
    if (stage.entered_at_ms == null && index !== 0) { problems.push(`stage ${index} was requested but never applied`); continue; }
    if (!(Number(stage.media_bytes) > 0)) {
      problems.push(`stage ${index} (${stage.label}, ${stage.kbps} kb/s) delivered no media through the proxy`);
    }
  }
  return problems;
}

if (require.main === module) {
  const [evidencePath, logPath, phase] = process.argv.slice(2);
  if (!evidencePath || !logPath || !phase) {
    console.error("usage: check-evidence.js EVIDENCE_JSON TEST_LOG PHASE");
    process.exit(2);
  }
  let evidence;
  try { evidence = JSON.parse(fs.readFileSync(evidencePath, "utf8")); } catch (error) {
    console.error(`UNSHAPED: cannot read shaper evidence ${evidencePath}: ${error.message}`);
    process.exit(1);
  }
  const problems = verify(evidence, fs.readFileSync(logPath, "utf8"), phase);
  if (problems.length) {
    console.error("UNSHAPED: this run is not evidence of anything through the shaper:");
    for (const problem of problems) console.error(`  - ${problem}`);
    process.exit(1);
  }
  console.log(`shaped: media delivered through the proxy in stages ${requestedStages(fs.readFileSync(logPath, "utf8"), phase).join(", ")}`);
}

module.exports = { requestedStages, verify };
