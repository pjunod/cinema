#!/usr/bin/env node
"use strict";
// Bounded, isolated measurements. Never points at a production server.
const fs = require("node:fs"),
  fsp = fs.promises,
  path = require("node:path");
const http = require("node:http"),
  cp = require("node:child_process"),
  crypto = require("node:crypto");
const {
  startServer,
  api,
  CdpDriver,
} = require("../../../../scripts/playback-lab");
const ROOT = path.resolve(__dirname, "../../../..");
const args = process.argv.slice(2),
  opt = {};
for (let i = 0; i < args.length; i += 2) {
  if (!args[i].startsWith("--") || !args[i + 1])
    throw Error("expected --key value");
  opt[args[i].slice(2)] = args[i + 1];
}
if (opt.matrix && !["full", "final-pair", "boundaries"].includes(opt.matrix))
  throw Error("matrix must be full, final-pair or boundaries");
if (opt.trigger && !["ended", "manual"].includes(opt.trigger))
  throw Error("trigger must be ended or manual");
if (!opt.server || !opt.output)
  throw Error("--server and --output are required");
const output = path.resolve(opt.output),
  scratch = path.join(output, "scratch");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function sha(p) {
  const hash = crypto.createHash("sha256"),
    fd = fs.openSync(p, "r"),
    buffer = Buffer.alloc(1024 * 1024);
  try {
    let n;
    while ((n = fs.readSync(fd, buffer, 0, buffer.length, null)) > 0)
      hash.update(buffer.subarray(0, n));
  } finally {
    fs.closeSync(fd);
  }
  return hash.digest("hex");
}
async function until(fn, ms, label) {
  const start = performance.now();
  let last;
  while (performance.now() - start < ms) {
    last = await fn();
    if (last) return last;
    await sleep(100);
  }
  throw Error("timeout " + label + " last=" + JSON.stringify(last));
}
const report = {
  schema: 1,
  harness_sha256: sha(__filename),
  playback_lab_sha256: sha(
    path.resolve(__dirname, "../../../../scripts/playback-lab"),
  ),
  ffprobe_sha256: process.env.PLURX_FFPROBE
    ? sha(process.env.PLURX_FFPROBE)
    : null,
  started_at: new Date().toISOString(),
  scope: "isolated_current_web_and_daemon_generated_episode_transition",
  matrix: opt.matrix || "full",
  source: cp
    .execFileSync("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8" })
    .trim(),
  binary_sha256: sha(opt.server),
  trials: [],
  checks: [],
  limits: [
    "Shared host has unrelated workloads; paired observations are not idle capacity or statistically significant fleet estimates.",
    "Generated local AVC/AAC media, not representative film or physical display qualification.",
    "Operating-system page cache uncontrolled; cold means no successor metadata preparation.",
    "Presentation callback means compositor submission, not physical panel acknowledgement.",
    "Metadata delay is an explicit network experiment input, never a measured fleet RTT.",
  ],
};
let ownsOutput = false;
let daemon,
  driver,
  proxy,
  requests = [],
  delayMs = 0,
  closing = false;
function hostLoad() {
  const processes = cp
    .execFileSync("ps", ["-ww", "-axo", "%cpu=,comm="], { encoding: "utf8" })
    .trim()
    .split("\n");
  return {
    load_average: require("node:os").loadavg(),
    cpu_count: require("node:os").cpus().length,
    competing_build_processes: processes
      .filter((r) =>
        /\b(rustc|cargo|cargo-clippy|clang|java|qemu-system-aarch64)\b/.test(r),
      )
      .map((r) => {
        const m = r.trim().match(/^([\d.]+)\s+(.*)$/);
        return { command: path.basename(m[2]), cpu_percent: Number(m[1]) };
      }),
  };
}
function resources() {
  const rows = cp
    .execFileSync("ps", ["-axo", "pid=,ppid=,rss=,time="], { encoding: "utf8" })
    .trim()
    .split("\n")
    .map((line) => line.trim().split(/\s+/));
  const ids = new Set([daemon.child.pid]);
  for (let n = 0; n < 8; n++)
    for (const r of rows) if (ids.has(Number(r[1]))) ids.add(Number(r[0]));
  const owned = rows
    .filter((r) => ids.has(Number(r[0])))
    .map((r) => ({
      pid: Number(r[0]),
      rss_bytes: Number(r[2]) * 1024,
      cpu_time: r[3],
    }));
  return {
    host_load: hostLoad(),
    daemon_tree: owned,
    rss_sum_bytes: owned.reduce((n, r) => n + r.rss_bytes, 0),
    scope:
      "endpoint process-tree samples, not interval peak or retired-child CPU",
  };
}
async function persist() {
  await fsp.writeFile(
    path.join(output, "receipt.json"),
    JSON.stringify(report, null, 2) + "\n",
  );
}
async function proxyStart() {
  proxy = http.createServer((req, res) => {
    const u = new URL(req.url, daemon.baseUrl),
      record = {
        path: u.pathname,
        method: req.method,
        start_ms: performance.now(),
        bytes: 0,
        status: null,
      };
    requests.push(record);
    if (
      opt["web-source"] &&
      req.method === "GET" &&
      u.pathname === "/assets/player/autoplay-next.js"
    ) {
      const body = fs.readFileSync(
        path.join(
          path.resolve(opt["web-source"]),
          "crates/plurxd/src/web/player/autoplay-next.js",
        ),
      );
      record.status = 200;
      record.bytes = body.length;
      record.end_ms = performance.now();
      res.writeHead(200, {
        "Content-Type": "application/javascript",
        "Cache-Control": "no-store",
      });
      res.end(body);
      return;
    }
    const forward = () => {
      if (res.destroyed) return;
      const upstream = http.request(
        u,
        { method: req.method, headers: { ...req.headers, host: u.host } },
        (reply) => {
          record.status = reply.statusCode;
          res.writeHead(reply.statusCode, reply.headers);
          reply.on("data", (chunk) => {
            record.bytes += chunk.length;
          });
          reply.on("end", () => {
            record.end_ms = performance.now();
          });
          reply.pipe(res);
        },
      );
      upstream.on("error", (e) => {
        record.error = e.code || e.message;
        if (!res.headersSent) res.writeHead(502);
        res.end();
      });
      req.pipe(upstream);
      res.on("close", () => upstream.destroy());
    };
    if (req.method === "GET" && /^\/api\/v1\/items\/[^/]+$/.test(u.pathname))
      setTimeout(forward, delayMs);
    else forward();
  });
  await new Promise((r) => proxy.listen(0, "127.0.0.1", r));
  return "http://127.0.0.1:" + proxy.address().port;
}
async function fixture() {
  await fsp.mkdir(path.join(scratch, "empty"), { recursive: true });
  const dir = path.join(scratch, "episodes");
  await fsp.mkdir(dir, { recursive: true });
  const base = path.join(scratch, "source.mp4");
  const argv = [
    "-hide_banner",
    "-loglevel",
    "error",
    "-y",
    "-f",
    "lavfi",
    "-i",
    "testsrc2=size=640x360:rate=24",
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=48000",
    "-t",
    "12",
    "-c:v",
    "libx264",
    "-threads",
    "2",
    "-preset",
    "veryfast",
    "-crf",
    "20",
    "-g",
    "48",
    "-bf",
    "0",
    "-pix_fmt",
    "yuv420p",
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-colorspace",
    "bt709",
    "-c:a",
    "aac",
    "-b:a",
    "96k",
    "-movflags",
    "+faststart",
    base,
  ];
  cp.execFileSync(process.env.PLURX_FFMPEG || "ffmpeg", argv, {
    stdio: "inherit",
    timeout: 60000,
  });
  for (const name of [
    "Qualification Show - S01E01.mp4",
    "Qualification Show - S01E02.mp4",
    "Qualification Show - S02E01.mp4",
  ])
    await fsp.copyFile(base, path.join(dir, name));
  report.fixture = {
    sha256: sha(base),
    bytes: fs.statSync(base).size,
    ffmpeg: cp
      .execFileSync(process.env.PLURX_FFMPEG || "ffmpeg", ["-version"], {
        encoding: "utf8",
      })
      .split("\n")[0],
    argv: argv.map((v) => (v === base ? "$SCRATCH/source.mp4" : v)),
  };
  return dir;
}
async function catalogue(dir) {
  const lib = await api(daemon.baseUrl, "/libraries", {
    token: daemon.token,
    method: "POST",
    body: {
      name: "Video latency qualification",
      kind: "shows",
      paths: [dir],
      anime: false,
    },
  });
  await until(
    async () => {
      const s = await api(daemon.baseUrl, "/scan/status", {
        token: daemon.token,
      });
      return s[String(lib.id)]?.last_scan && !s[String(lib.id)].running;
    },
    180000,
    "tv scan",
  );
  const listing = await api(
    daemon.baseUrl,
    `/libraries/${lib.id}/items?limit=200&sort=title`,
    { token: daemon.token },
  );
  const episodes = [];
  async function descend(item) {
    const d = await api(daemon.baseUrl, `/items/${item.id}`, {
      token: daemon.token,
    });
    if (d.item.kind === "episode") episodes.push(d);
    else for (const c of d.children || []) await descend(c);
  }
  for (const i of listing.items) await descend(i);
  episodes.sort((a, b) =>
    a.files[0].filename.localeCompare(b.files[0].filename),
  );
  report.catalogue = episodes.map((d) => ({
    item_id: String(d.item.id),
    file_id: String(d.files[0].id),
    filename: d.files[0].filename,
    kind: d.item.kind,
  }));
  if (episodes.length !== 3)
    throw Error("expected three episodes " + JSON.stringify(report.catalogue));
  return episodes;
}
async function setupBrowser(url) {
  driver = new CdpDriver({ headless: true }, url, scratch, "chrome");
  await driver.start();
  await driver.exec(
    `localStorage.setItem('plurx_token',${JSON.stringify(daemon.token)});localStorage.setItem('plurx_autonext','1');localStorage.setItem('plurx_autoskip','0');localStorage.setItem('plurx_quality','original');return true;`,
  );
  await driver.reload();
  await until(
    () =>
      driver.eval("typeof watchPrepare==='function'&&typeof CAPS_Q==='string'"),
    30000,
    "app",
  );
  report.browser = await driver.eval(
    "({user_agent:navigator.userAgent,caps:PLAY_CAPS})",
  );
  await driver.exec(
    `window.__qualOriginalPrepare=prepareNextEpisodeIfNearEnd;window.__qualFrames=[];window.__qualFrameSequence=[];window.__qualErrors=[];window.__qualCapture=true; const attach=()=>{const v=document.getElementById('video');if(v&&!v.__qualAttached){v.__qualAttached=true;let previous=null;const frame=(at,m)=>{if(!window.__qualCapture)return;const id=PLAYER?.fileId;if(document.getElementById('video')!==v||!playbackOwnsAttachedMedia(PLAYER)){v.requestVideoFrameCallback(frame);return;}if(id!==previous||window.__qualFrames.length===0){window.__qualFrames.push({file_id:String(id),attempt_id:PLAYER?.attemptId,current_src:v.currentSrc.replace(/([?&](?:token|api_key)=)[^&]+/g,"$1<redacted>"),at_ms:at,media_time:m.mediaTime,presentation_time_ms:m.presentationTime,expected_display_time_ms:m.expectedDisplayTime});previous=id;}window.__qualLastFrame={file_id:String(id),attempt_id:PLAYER?.attemptId,current_src:v.currentSrc.replace(/([?&](?:token|api_key)=)[^&]+/g,"$1<redacted>"),at_ms:at,media_time:m.mediaTime,presentation_time_ms:m.presentationTime,expected_display_time_ms:m.expectedDisplayTime};if(window.__qualFrameSequence.length<4096)window.__qualFrameSequence.push(window.__qualLastFrame);v.requestVideoFrameCallback(frame);};v.requestVideoFrameCallback(frame);v.addEventListener('error',()=>window.__qualErrors.push({code:v.error?.code,message:v.error?.message}));}};window.__qualAttachTimer=setInterval(attach,50);attach();return true;`,
  );
}
async function oneTrial(ep, next, mode, pair, delay) {
  delayMs = delay;
  const resourceBefore = resources();
  const requestStart = requests.length;
  await driver.exec(
    `await closePlayer();prepareNextEpisodeIfNearEnd=${mode === "prepared" ? "window.__qualOriginalPrepare" : "()=>{}"};setAutoNext(true);window.__qualFrames=[];window.__qualFrameSequence=[];window.__qualErrors=[];return true;`,
  );
  await sleep(300);
  await driver.exec(
    `location.hash=${JSON.stringify("#/item/")}+${JSON.stringify(String(ep.item.id))};return true;`,
  );
  await until(
    () =>
      driver.eval(
        `WATCH_ITEM_PAGE?.id===${JSON.stringify(String(ep.item.id))}&&location.hash===${JSON.stringify("#/item/" + String(ep.item.id))}`,
      ),
    10000,
    "item route",
  );
  await driver.exec(
    `const p=WATCH_ITEM_PAGE;await play(p.playable.id,p.item.title,0,p.playable.duration_ms,Object.assign(playbackMetaFor(p,p.playable),{_watchPage:p}));return true;`,
  );
  await until(
    () =>
      driver.eval(
        `window.__qualFrames.some(f=>f.file_id===${JSON.stringify(String(ep.files[0].id))})`,
      ),
    60000,
    "outgoing frame",
  );
  if (mode === "prepared")
    await until(
      () => driver.eval("!!PLAYER?.nextEpisodePreparation?.page"),
      10000,
      "prewarm",
    );
  await sleep(300);
  const before = await driver.eval(
    `({frames:window.__qualFrames,last:window.__qualLastFrame,prepared:!!PLAYER?.nextEpisodePreparation?.page,at_ms:performance.now(),file_id:String(PLAYER?.fileId),stalls:PLAYBACK_LIFETIME_STALLS})`,
  );
  const transitionRequestIndex = requests.length;
  let trigger;
  if (opt.trigger === "ended") {
    await driver.exec(
      `window.__qualTrigger=null;document.getElementById('video').addEventListener('ended',()=>{window.__qualTrigger={at_ms:performance.now(),last:window.__qualLastFrame};},{once:true});return true;`,
    );
    trigger = await until(
      () => driver.eval("window.__qualTrigger"),
      30000,
      "natural episode end",
    );
  } else {
    trigger = await driver.exec(
      `window.__qualTrigger={at_ms:performance.now(),last:window.__qualLastFrame};playNextEpisode();return window.__qualTrigger;`,
    );
  }
  await until(
    () =>
      driver.eval(
        `window.__qualFrames.some(f=>f.file_id===${JSON.stringify(String(next.files[0].id))})`,
      ),
    60000,
    "successor frame",
  );
  await sleep(750);
  const after = await driver.eval(
    `({frames:window.__qualFrames,sequence:window.__qualFrameSequence,errors:window.__qualErrors,stalls:PLAYBACK_LIFETIME_STALLS,at_ms:performance.now(),file_id:String(PLAYER?.fileId),last:window.__qualLastFrame})`,
  );
  const first = after.frames.find(
    (f) => f.file_id === String(next.files[0].id),
  );
  const lastOutgoing = after.sequence
    .filter(
      (f) =>
        f.file_id === String(ep.files[0].id) &&
        f.presentation_time_ms < first.presentation_time_ms,
    )
    .at(-1);
  const q = requests.slice(requestStart);
  const transitionRequests = requests.slice(transitionRequestIndex);
  const row = {
    resources_before: resourceBefore,
    resources_after: resources(),
    pair,
    mode,
    trigger_kind: opt.trigger === "ended" ? "natural_ended" : "manual_next",
    metadata_response_delay_ms: delay,
    from: ep.files[0].filename,
    to: next.files[0].filename,
    prepared: before.prepared,
    transition_requests: transitionRequests.map((r) => ({
      path: r.path,
      method: r.method,
      status: r.status,
      bytes: r.bytes,
    })),
    trigger,
    first_successor_frame: first,
    trigger_to_frame_ms: first.at_ms - trigger.at_ms,
    last_outgoing_frame: lastOutgoing,
    last_outgoing_to_first_successor_ms:
      first.presentation_time_ms - lastOutgoing.presentation_time_ms,
    frame_sequence: after.sequence,
    errors: after.errors,
    new_stalls: after.stalls - before.stalls,
    requests: q.map((r) => ({
      ...r,
      start_ms: r.start_ms - q[0].start_ms,
      end_ms: r.end_ms == null ? null : r.end_ms - q[0].start_ms,
    })),
    metadata_bytes: q
      .filter((r) => /^\/api\/v1\/items\//.test(r.path))
      .reduce((n, r) => n + r.bytes, 0),
    media_bytes: q
      .filter((r) => /\/(direct|stream|hls|vod)(?:\/|$)/.test(r.path))
      .reduce((n, r) => n + r.bytes, 0),
  };
  report.trials.push(row);
  await persist();
  console.log(
    JSON.stringify({
      pair,
      mode,
      delay,
      ttff: row.trigger_to_frame_ms,
      gap: row.last_outgoing_to_first_successor_ms,
    }),
  );
}
async function boundaryChecks(episodes) {
  await driver.exec(
    `window.__qualLifecycle=[];window.__qualCancel=cancelNextEpisodePreparation;cancelNextEpisodePreparation=function(p){const before=!!p?.nextEpisodePreparation;const result=window.__qualCancel(p);window.__qualLifecycle.push({kind:"cancel",before,after:!!p?.nextEpisodePreparation,at:performance.now()});return result;};window.__qualOriginalPrepare=((original)=>function(p,v){const before=p?.nextEpisodePreparation;const result=original(p,v);if(p?.nextEpisodePreparation&&p.nextEpisodePreparation!==before)window.__qualLifecycle.push({kind:"prepare",wants:p.wantsPlayback,paused:v.paused,watch:!!WATCH,closing:!!WATCH_CLOSE_PROMISE,at:performance.now(),stack:new Error().stack});return result;})(window.__qualOriginalPrepare);return true;`,
  );
  async function launch(ep, enabled) {
    await driver.exec(
      `await closePlayer();prepareNextEpisodeIfNearEnd=window.__qualOriginalPrepare;setAutoNext(${enabled});window.__qualFrames=[];window.__qualFrameSequence=[];location.hash=${JSON.stringify("#/item/" + String(ep.item.id))};return true;`,
    );
    await until(
      () =>
        driver.eval(
          `WATCH_ITEM_PAGE?.id===${JSON.stringify(String(ep.item.id))}`,
        ),
      10000,
      "check item route",
    );
    await driver.exec(
      `const p=WATCH_ITEM_PAGE;await play(p.playable.id,p.item.title,0,p.playable.duration_ms,Object.assign(playbackMetaFor(p,p.playable),{_watchPage:p}));return true;`,
    );
    await until(
      () =>
        driver.eval(
          `window.__qualFrames.some(f=>f.file_id===${JSON.stringify(String(ep.files[0].id))})`,
        ),
      60000,
      "check outgoing frame",
    );
  }
  delayMs = 0;
  await launch(episodes[2], true);
  const terminal = await driver.exec(
    `await PLAYER.nextEpisodePreparation?.promise;return {state:!!PLAYER.nextEpisodePreparation,page:!!PLAYER.nextEpisodePreparation?.page};`,
  );
  const terminalStart = requests.length;
  await sleep(1500);
  const terminalReads = requests
    .slice(terminalStart)
    .filter((r) => /^\/api\/v1\/items\//.test(r.path));
  report.checks.push({
    name: "end_of_series_no_successor_no_repeated_metadata",
    result:
      terminal.state && !terminal.page && terminalReads.length === 0
        ? "passed"
        : "failed",
    state: terminal,
    extra_metadata_reads: terminalReads.length,
  });
  await launch(episodes[0], false);
  const offStart = requests.length;
  await sleep(1500);
  const off = await driver.eval(
    "({state:!!PLAYER.nextEpisodePreparation,enabled:autoNextOn()})",
  );
  report.checks.push({
    name: "autoplay_off_does_not_prepare",
    result: !off.state && !off.enabled ? "passed" : "failed",
    state: off,
    requests: requests
      .slice(offStart)
      .map((r) => ({ path: r.path, method: r.method, bytes: r.bytes })),
  });
  // Begin only after normal watch-page work has settled, so the cancellation
  // cell isolates the background metadata owner rather than initial navigation.
  delayMs = 1500;
  await driver.exec(
    `setAutoNext(true);prepareNextEpisodeIfNearEnd(PLAYER,document.getElementById('video'));return true;`,
  );
  const cancellation = await driver.eval(
    "({state:!!PLAYER.nextEpisodePreparation,page:!!PLAYER.nextEpisodePreparation?.page})",
  );
  const cancelStart = requests.length;
  await driver.exec(`await closePlayer();return true;`);
  await sleep(1800);
  const post = await driver.eval(
    "({active:!!PLAYER?.wantsPlayback,prepared:!!PLAYER?.nextEpisodePreparation,ready:!!AUTOPLAY_NEXT_PREPARED,session:PLAYER?.sessionId,watch:!!WATCH,current:PLAYER?.nextEpisodePreparation?.current(),page:!!PLAYER?.nextEpisodePreparation?.page,lifecycle:window.__qualLifecycle})",
  );
  const postRequests = requests
    .slice(cancelStart)
    .filter(
      (r) =>
        r.method === "POST" && /(hls|sessions|playback|offline)/.test(r.path),
    );
  report.checks.push({
    name: "navigation_close_cancels_inflight_preparation",
    result:
      cancellation.state &&
      !post.active &&
      !post.prepared &&
      !post.ready &&
      postRequests.length === 0
        ? "passed"
        : "failed",
    before: cancellation,
    after: post,
    post_cancel_starts: postRequests.map((r) => ({
      path: r.path,
      method: r.method,
      status: r.status,
    })),
  });
  delayMs = 0;
  await persist();
}
async function main() {
  await fsp.mkdir(output);
  ownsOutput = true;
  if (opt["web-source"])
    report.web_overlay = {
      file: "crates/plurxd/src/web/player/autoplay-next.js",
      sha256: sha(
        path.join(
          path.resolve(opt["web-source"]),
          "crates/plurxd/src/web/player/autoplay-next.js",
        ),
      ),
    };
  const dir = await fixture();
  daemon = await startServer(
    { server: opt.server, server_log: "info", startup_timeout_ms: 180000 },
    path.join(scratch, "empty"),
  );
  await api(daemon.baseUrl, "/settings", {
    token: daemon.token,
    method: "PUT",
    body: {
      vod_index_mins: 0,
      chapter_thumbnails: false,
      subtitle_backfill: false,
      cache_produce_mins: 0,
      probe_retry_mins: 0,
      artwork_retry_mins: 0,
    },
  });
  report.decode_facts_identity_available = !fs
    .readFileSync(daemon.logFile, "utf8")
    .includes("FFprobe identity is unavailable");
  if (opt.matrix === "final-pair" && !report.decode_facts_identity_available)
    throw Error(
      "final release pair requires completed parser identity, not a detached compilation",
    );
  const episodes = await catalogue(dir);
  await sleep(3000);
  await until(
    async () => {
      const r = resources();
      return (
        /caption graph probe (complete|unavailable)/.test(
          fs.readFileSync(daemon.logFile, "utf8"),
        ) && r.daemon_tree.length === 1
      );
    },
    300000,
    "isolated daemon idle after background startup proof",
  );
  const url = await proxyStart();
  await setupBrowser(url);
  const pairs = Number(opt.pairs || 3);
  if (!Number.isInteger(pairs) || pairs < 1 || pairs > 6)
    throw Error("pairs must be 1..6");
  if (opt.matrix === "final-pair") {
    for (const mode of ["unprepared", "prepared"])
      await oneTrial(episodes[0], episodes[1], mode, 0, 80);
  } else if (opt.matrix !== "boundaries") {
    for (const delay of [0, 80])
      for (let pair = 0; pair < pairs; pair++)
        for (const mode of pair % 2
          ? ["prepared", "unprepared"]
          : ["unprepared", "prepared"])
          await oneTrial(episodes[0], episodes[1], mode, pair, delay);
    for (let pair = 0; pair < 2; pair++)
      for (const mode of pair % 2
        ? ["prepared", "unprepared"]
        : ["unprepared", "prepared"])
        await oneTrial(episodes[1], episodes[2], mode, pair, 80);
  }
  await boundaryChecks(episodes);
  report.result = report.checks.every((r) => r.result === "passed")
    ? "completed"
    : "failed_boundary_check";
}
main()
  .catch(async (e) => {
    report.browser_failure_state = driver
      ? await driver
          .eval(
            `({file_id:PLAYER?.fileId,attempt:PLAYER?.attemptId,pending:PLAYER?.pendingMediaChange,pending_open:PLAYER?.pendingOpenAttempt,frames:window.__qualFrames,errors:window.__qualErrors,prepared:!!PLAYER?.nextEpisodePreparation?.page,hash:location.hash,watch:WATCH?{accepted:WATCH.accepted,load:WATCH.load}:null,video:(()=>{const v=document.getElementById('video');return v?{paused:v.paused,seeking:v.seeking,time:v.currentTime,ready:v.readyState,ended:v.ended,error:v.error?.message}:null})()})`,
          )
          .catch(() => null)
      : null;
    report.last_requests = requests.slice(-25).map((r) => ({
      path: r.path,
      method: r.method,
      status: r.status,
      bytes: r.bytes,
    }));
    report.result = "failed";
    report.error = e.stack;
    console.error(e.stack);
    process.exitCode = 1;
  })
  .finally(async () => {
    closing = true;
    if (driver) await driver.close().catch(() => {});
    if (proxy) {
      proxy.closeAllConnections();
      await new Promise((r) => proxy.close(r));
    }
    if (daemon) {
      await fsp
        .copyFile(daemon.logFile, path.join(output, "daemon.log"))
        .catch(() => {});
      await daemon.close(false).catch(() => {});
    }
    if (ownsOutput) await persist();
    if (ownsOutput) await fsp.rm(scratch, { recursive: true, force: true });
  });
