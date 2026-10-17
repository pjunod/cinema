#!/usr/bin/env node
// Shape a real plurx server for the installed shipping Apple app under XCUITest.
// Link stages advance only when the UI test logs a link_request (the on-device
// runner has no local-network permission, so the Mac applies each stage).
"use strict";
const path = require("node:path");
const fs = require("node:fs");
const crypto = require("node:crypto");
const lab = require(path.join(__dirname, "..", "playback-lab"));

const [target, port, publicHost, outDir, followLog] = process.argv.slice(2);
if (!target || !port || !publicHost || !outDir) {
  console.error("usage: proxy.js TARGET_ORIGIN PORT PUBLIC_HOST OUT_DIR [FOLLOW_LOG]");
  process.exit(2);
}
const kbps = [100000, 10000, 25000, 10000, 25000, 3000, 25000];
const labels = ["start", "pressure", "recovery", "pressure", "recovery", "low", "rebound"];
const token = crypto.randomBytes(32).toString("hex");
const proxy = new lab.ShapingProxy({
  id: "cq-native-fixed-links", spec: "100/10/25/10/25/3/25 Mb/s",
  description: "CQ fixed links plus a 3 Mb/s downshift and 25 Mb/s recovery",
  stages: kbps.map((k, index) => ({ index, label: labels[index], kbps: k })),
}, target, { listenHost: "0.0.0.0", listenPort: Number(port), publicHost, controlToken: token, autoAdvance: false });

(async () => {
  const base = await proxy.start();
  fs.mkdirSync(outDir, { recursive: true });
  fs.writeFileSync(path.join(outDir, "control.json"), JSON.stringify({ proxy: base, target, token }), { mode: 0o600 });
  process.stdout.write(`PROXY ${base} -> ${target}\n`);
  if (followLog) {
    let offset = 0;
    setInterval(() => {
      let size = 0;
      try { size = fs.statSync(followLog).size; } catch { return; }
      if (size < offset) offset = 0;
      if (size === offset) return;
      const fd = fs.openSync(followLog, "r");
      const buf = Buffer.alloc(size - offset);
      fs.readSync(fd, buf, 0, buf.length, offset); fs.closeSync(fd); offset = size;
      for (const line of buf.toString("utf8").split("\n")) {
        const m = /CQEVT (\{.*\})/.exec(line);
        if (!m) continue;
        let e; try { e = JSON.parse(m[1]); } catch { continue; }
        if (e.kind !== "link_request") continue;
        while (proxy.stageIndex < e.stage) {
          const at = proxy.applyCliff(`test-stage-${e.stage}`);
          if (at === null) break;
          process.stdout.write(`CLIFF stage=${proxy.stageIndex} kbps=${proxy.stages[proxy.stageIndex].kbps} at_ms=${at}\n`);
        }
      }
    }, 250);
  }
  const finish = async () => {
    fs.writeFileSync(path.join(outDir, "shaper-evidence.json"), JSON.stringify(proxy.controlSnapshot(), null, 2));
    await proxy.close().catch(() => {});
    fs.rmSync(path.join(outDir, "control.json"), { force: true });
    process.exit(0);
  };
  process.once("SIGTERM", finish);
  process.once("SIGINT", finish);
})().catch((error) => { console.error(error); process.exit(1); });
