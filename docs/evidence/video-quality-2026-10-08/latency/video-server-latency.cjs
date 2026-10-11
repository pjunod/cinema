#!/usr/bin/env node
"use strict";
const fs = require("node:fs"),
  fsp = fs.promises,
  path = require("node:path"),
  cp = require("node:child_process"),
  crypto = require("node:crypto");
const {
  startServer,
  api,
  fragmentIndexPassBuiltFile,
} = require("../../../../scripts/playback-lab");
const opt = {};
for (let i = 2; i < process.argv.length; i += 2)
  opt[process.argv[i].replace(/^--/, "")] = process.argv[i + 1];
if (opt.case && !["probe", "hls", "all"].includes(opt.case))
  throw Error("case must be probe, hls or all");
if (!opt.current || !opt.control || !opt.output)
  throw Error("--current --control --output required");
const out = path.resolve(opt.output),
  scratch = path.join(out, "scratch"),
  ffmpeg = process.env.PLURX_FFMPEG || "ffmpeg";
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
const digest = (b) => crypto.createHash("sha256").update(b).digest("hex");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
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
  kind: opt.case || "probe",
  control_design: opt["control-design"] || "see artifact identity",
  hls_source: opt["hls-source"] || "encoded",
  source: cp
    .execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" })
    .trim(),
  artifacts: { current: sha(opt.current), control: sha(opt.control) },
  trials: [],
  limits: [
    "Shared host has unrelated workloads; paired observations are not idle capacity or statistically significant fleet estimates.",
    "Isolated generated AVC/AAC, software x264, loopback and OS-cache-uncontrolled source.",
    "Startup endpoint is first decoded video frame in common FFmpeg controller, not native UI/display.",
    "Resource samples measure daemon process-tree endpoints/interval sample maximum, not allocator high water or retired-child CPU.",
  ],
};
const fixtureCount = report.kind === "hls" ? 1 : 4;
const servers = {};
let ownsOutput = false;
async function save() {
  await fsp.writeFile(
    path.join(out, "receipt.json"),
    JSON.stringify(report, null, 2) + "\n",
  );
}
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
function resources(s) {
  const rows = cp
    .execFileSync("ps", ["-axo", "pid=,ppid=,rss=,time="], { encoding: "utf8" })
    .trim()
    .split("\n")
    .map((r) => r.trim().split(/\s+/));
  const ids = new Set([s.child.pid]);
  for (let n = 0; n < 8; n++)
    for (const r of rows) if (ids.has(+r[1])) ids.add(+r[0]);
  const tree = rows
    .filter((r) => ids.has(+r[0]))
    .map((r) => ({ pid: +r[0], rss_bytes: +r[2] * 1024, cpu_time: r[3] }));
  return {
    host_load: hostLoad(),
    tree,
    rss_sum_bytes: tree.reduce((n, r) => n + r.rss_bytes, 0),
  };
}
async function generate() {
  await fsp.mkdir(scratch, { recursive: true });
  const dir = path.join(scratch, "media");
  await fsp.mkdir(dir);
  const source = path.join(dir, "cold-0.mp4");
  const argv = [
    "-hide_banner",
    "-loglevel",
    "error",
    "-y",
    "-f",
    "lavfi",
    "-i",
    "testsrc2=size=960x540:rate=24",
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:sample_rate=48000",
    "-t",
    "16",
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
    source,
  ];
  cp.execFileSync(ffmpeg, argv, { stdio: "inherit", timeout: 60000 });
  for (let i = 1; i < fixtureCount; i++)
    await fsp.copyFile(source, path.join(dir, `cold-${i}.mp4`));
  report.fixture = {
    sha256: sha(source),
    bytes: fs.statSync(source).size,
    files: fixtureCount,
    argv: argv.map((v) => (v === source ? "$MEDIA/cold-0.mp4" : v)),
    ffmpeg_sha256: sha(ffmpeg),
  };
  return dir;
}
async function create(s, file, start, sourceCopy = false) {
  const body = {
    playback_id: crypto.randomUUID(),
    request_id: crypto.randomUUID(),
    height: sourceCopy ? 540 : 360,
    quality_auto: false,
    start,
    copy: sourceCopy,
    aac: !sourceCopy,
    presentation: "vod",
    transport: "hlsjs",
    block_budget_secs: 30,
  };
  const session = await api(s.baseUrl, `/files/${file.id}/hls/sessions`, {
    token: s.token,
    method: "POST",
    body,
    timeout: 90000,
  });
  if (!session.vod || !(sourceCopy ? /^(vod|copy)$/i : /software|x264/i).test(session.encoder))
    throw Error("wrong delivered session " + JSON.stringify(session));
  return session;
}
function timedDecode(url, token) {
  return new Promise((resolve, reject) => {
    const start = performance.now();
    const argv = [
      "-hide_banner",
      "-loglevel",
      "error",
      "-headers",
      "Authorization: Bearer " + token + "\r\n",
      "-i",
      url.toString(),
      "-map",
      "0:v:0",
      "-an",
      "-frames:v",
      "1",
      "-f",
      "null",
      "-",
    ];
    const child = cp.spawn(ffmpeg, argv, {
      stdio: ["ignore", "ignore", "pipe"],
    });
    let stderr = "";
    child.stderr.on(
      "data",
      (b) => (stderr = (stderr + b.toString()).slice(-16384)),
    );
    const timer = setTimeout(() => child.kill("SIGKILL"), 60000);
    child.on("error", reject);
    child.on("close", (code) => {
      clearTimeout(timer);
      if (code !== 0)
        reject(
          Error(
            "decode failed " +
              stderr.replace(/([?&]token=)[^&\s]+/g, "$1<redacted>"),
          ),
        );
      else resolve({ decoder_ms: performance.now() - start });
    });
  });
}
function authUrl(s, url) {
  const u = new URL(url, s.baseUrl);
  u.searchParams.set("token", s.token);
  return u;
}
async function probeMetrics(s) {
  const r = await fetch(new URL("/metrics", s.baseUrl), {
    signal: AbortSignal.timeout(10000),
    headers: { authorization: "Bearer " + s.token },
  });
  if (!r.ok) throw Error("metrics unavailable " + r.status);
  const lines = (await r.text())
    .split("\n")
    .filter((l) =>
      /^plurx_decode_facts_(phase_seconds_(count|sum)|lookups_total)/.test(l),
    );
  if (!lines.length) throw Error("decode-fact metrics absent");
  return Object.fromEntries(
    lines.map((l) => {
      const i = l.lastIndexOf(" ");
      return [l.slice(0, i), Number(l.slice(i + 1))];
    }),
  );
}
async function startup(s, name, file, condition, pair) {
  const metricsBefore = await probeMetrics(s);
  const before = resources(s),
    start = performance.now();
  const session = await create(s, file, condition === "resume" ? 4 : 0),
    created = performance.now();
  try {
    const decoded = await timedDecode(
      authUrl(s, session.playlist_url),
      s.token,
    );
    const firstFrameAt = performance.now();
    const metricsAfter = await probeMetrics(s);
    const row = {
      observed_at: new Date().toISOString(),
      variant: name,
      pair,
      condition,
      file: file.filename,
      create_ms: created - start,
      create_interval_excludes_hls_get: true,
      first_frame_attribution: report.control_design.includes("combined")
        ? "combined probe and body-owner control, not probe-only"
        : "see control identity",
      create_to_first_decoded_frame_ms: firstFrameAt - start,
      probe_metric_delta: Object.fromEntries(
        Object.entries(metricsAfter).map(([k, v]) => [
          k,
          v - (metricsBefore[k] || 0),
        ]),
      ),
      ...decoded,
      session: {
        vod: session.vod,
        encoder: session.encoder,
        height: session.height,
      },
      resource_before: before,
      resource_after: resources(s),
    };
    report.trials.push(row);
    await save();
    const collections =
      row.probe_metric_delta[
        'plurx_decode_facts_phase_seconds_count{phase="collection",outcome="ok"}'
      ];
    const expected = name === "control" && condition === "cold" ? 2 : 1;
    if (collections !== expected)
      throw Error(
        "counterfactual was not exercised: " +
          name +
          " " +
          condition +
          " collections=" +
          collections +
          " expected=" +
          expected,
      );
    console.log(
      JSON.stringify({
        observed_at: new Date().toISOString(),
        variant: name,
        pair,
        condition,
        create_ms: row.create_ms,
        first_frame_ms: row.create_to_first_decoded_frame_ms,
      }),
    );
  } finally {
    await api(s.baseUrl, `/hls/${session.session_id}`, {
      token: s.token,
      method: "DELETE",
    }).catch(() => {});
  }
}
async function warmObject(s, file, name) {
  const session = await create(s, file, 0, opt["hls-source"] === "copy");
  let url = authUrl(s, session.playlist_url);
  const playlists = [];
  for (let depth = 0; depth < 4; depth++) {
    const response = await fetch(url, { signal: AbortSignal.timeout(90000) });
    if (!response.ok) throw Error("warm response " + response.status);
    const body = Buffer.from(await response.arrayBuffer());
    if (body.subarray(0, 7).toString() === "#EXTM3U") {
      const text = body.toString();
      playlists.push({ depth, sha256: digest(body), bytes: body.length });
      const uri = text.split(/\r?\n/).find((l) => l && !l.startsWith("#"));
      if (!uri) throw Error("empty media playlist");
      url = authUrl(s, new URL(uri, url));
      continue;
    }
    const firstBox = body.subarray(4, 8).toString();
    if (body.length < 8 || !["ftyp", "styp", "sidx", "moof"].includes(firstBox))
      throw Error("warm object is not an ISO media fragment");
    await fsp.writeFile(path.join(out, name + "-warm-object.m4s"), body);
    return {
      session,
      url,
      bytes: body.length,
      sha256: digest(body),
      playlists,
      first_box: firstBox,
    };
  }
  throw Error("playlist nesting exceeded bounded traversal");
}
async function hlsBatch(s, name, obj, pair, concurrency) {
  const metricsBefore = await probeMetrics(s);
  const before = resources(s),
    samples = [before],
    requests = [];
  const sampler = setInterval(() => {
    samples.push(resources(s));
  }, 100);
  const started = performance.now();
  try {
    let next = 0;
    await Promise.all(
      Array.from({ length: concurrency }, async () => {
        while (next < 200) {
          const index = next++,
            at = performance.now();
          const r = await fetch(obj.url, {
              signal: AbortSignal.timeout(10000),
            }),
            b = Buffer.from(await r.arrayBuffer());
          if (!r.ok || b.length !== obj.bytes || digest(b) !== obj.sha256)
            throw Error("response bytes/identity mismatch");
          requests.push({ index, ms: performance.now() - at, bytes: b.length });
        }
      }),
    );
  } finally {
    clearInterval(sampler);
  }
  samples.push(resources(s));
  const row = {
    observed_at: new Date().toISOString(),
    variant: name,
    pair,
    concurrency,
    requests,
    elapsed_ms: performance.now() - started,
    bytes: obj.bytes * requests.length,
    object_sha256: obj.sha256,
    resource_before: before,
    resource_after: samples.at(-1),
    sampled_max_tree_rss_bytes: Math.max(
      ...samples.map((s) => s.rss_sum_bytes),
    ),
    resource_sample_count: samples.length,
    source_probe_metric_delta: Object.fromEntries(
      Object.entries(await probeMetrics(s)).map(([k, v]) => [
        k,
        v - (metricsBefore[k] || 0),
      ]),
    ),
  };
  report.trials.push(row);
  await save();
  if (Object.values(row.source_probe_metric_delta).some((v) => v !== 0))
    throw Error("source-probe control was active inside HLS serving interval");
  console.log(
    JSON.stringify({
      observed_at: new Date().toISOString(),
      variant: name,
      pair,
      concurrency,
      elapsed_ms: row.elapsed_ms,
      rss: row.sampled_max_tree_rss_bytes,
    }),
  );
}
async function idle(s) {
  let consecutive = 0;
  const started = performance.now();
  while (performance.now() - started < 300000) {
    const captionsSettled = /caption graph probe (complete|unavailable)/.test(
      fs.readFileSync(s.logFile, "utf8"),
    );
    if (captionsSettled && resources(s).tree.length === 1) consecutive++;
    else consecutive = 0;
    if (consecutive >= 3) return;
    await sleep(500);
  }
  throw Error("isolated daemon did not quiesce before measurement");
}
async function main() {
  await fsp.mkdir(out);
  ownsOutput = true;
  const media = await generate();
  for (const name of ["current", "control"]) {
    const empty = path.join(scratch, "empty-" + name);
    await fsp.mkdir(empty);
    servers[name] = await startServer(
      { server: opt[name], server_log: "info", startup_timeout_ms: 180000 },
      empty,
    );
    const s = servers[name];
    const identityUnavailable = fs
      .readFileSync(s.logFile, "utf8")
      .includes("FFprobe identity is unavailable");
    report.source_probe_identity_unavailable ||= {};
    report.source_probe_identity_unavailable[name] = identityUnavailable;
    if (report.kind !== "hls" && identityUnavailable)
      throw Error(
        "Source-probe identity unavailable: no probe A/B claim is possible from this binary/tool package",
      );
    await api(s.baseUrl, "/settings", {
      token: s.token,
      method: "PUT",
      body: {
        vod_index_mins: 0,
        chapter_thumbnails: false,
        subtitle_backfill: false,
        probe_retry_mins: 0,
        artwork_retry_mins: 0,
        cache_produce_mins: 0,
      },
    });
    const library = await api(s.baseUrl, "/libraries", {
      token: s.token,
      method: "POST",
      body: {
        name: "Latency fixtures",
        kind: "home",
        paths: [media],
        anime: false,
      },
    });
    const scanAt = performance.now();
    while (true) {
      const statuses = await api(s.baseUrl, "/scan/status", { token: s.token });
      const status = statuses[String(library.id)];
      if (status && !status.running && status.last_scan) break;
      if (performance.now() - scanAt > 180000)
        throw Error("fixture scan timeout");
      await sleep(500);
    }
    const items = await api(
      s.baseUrl,
      `/libraries/${library.id}/items?limit=200&sort=title`,
      { token: s.token },
    );
    s.files = new Map();
    for (const item of items.items) {
      const detail = await api(s.baseUrl, `/items/${item.id}`, {
        token: s.token,
      });
      for (const file of detail.files || []) s.files.set(file.filename, file);
    }
    if (s.files.size !== fixtureCount)
      throw Error("unexpected independent fixture file count");
    await api(s.baseUrl, "/settings", {
      token: s.token,
      method: "PUT",
      body: {
        vod_presentation: true,
        vod_index_mins: 15,
        vod_block_budget_secs: "8",
        vod_materialize_budget_secs: "30",
      },
    });
    console.log("provisioning parser/index " + name);
    const provisionAt = performance.now();
    while (true) {
      const logs = await api(s.baseUrl, "/system/logs?level=debug&limit=500", {
        token: s.token,
      });
      if (
        [...s.files.values()].every((file) =>
          logs.some((entry) => fragmentIndexPassBuiltFile(entry, file.id)),
        )
      )
        break;
      if (performance.now() - provisionAt > 900000)
        throw Error("bounded 15min parser/index provisioning timeout");
      await sleep(1000);
    }
    report.provisioning ||= {};
    report.provisioning[name] = {
      parser_and_index_ms: performance.now() - provisionAt,
      excluded_from_startup_trials: true,
    };
    await api(servers[name].baseUrl, "/settings", {
      token: servers[name].token,
      method: "PUT",
      body: {
        vod_index_mins: 0,
        transcode_software_pool_threads: 2,
        vod_reorder_frames: 0,
        chapter_thumbnails: false,
        subtitle_backfill: false,
        cache_produce_mins: 0,
        probe_retry_mins: 0,
        artwork_retry_mins: 0,
      },
    });
    console.log("ready " + name);
  }
  for (const s of Object.values(servers)) await idle(s);
  if (report.kind !== "hls") {
    for (let pair = 0; pair < fixtureCount; pair++)
      for (const condition of ["cold", "warm", "resume"])
        for (const name of pair % 2
          ? ["control", "current"]
          : ["current", "control"])
          await startup(
            servers[name],
            name,
            servers[name].files.get(`cold-${pair}.mp4`),
            condition,
            pair,
          );
  }
  if (report.kind !== "probe") {
    const objects = {};
    for (const name of ["current", "control"])
      objects[name] = await warmObject(
        servers[name],
        servers[name].files.get("cold-0.mp4"),
        name,
      );
    report.warm_objects = Object.fromEntries(
      Object.entries(objects).map(([k, v]) => [
        k,
        {
          bytes: v.bytes,
          sha256: v.sha256,
          first_box: v.first_box,
          playlists: v.playlists,
          session: {
            vod: v.session.vod,
            encoder: v.session.encoder,
            height: v.session.height,
            start_seconds: v.session.start_seconds,
          },
        },
      ]),
    );
    if (objects.current.sha256 !== objects.control.sha256)
      throw Error("warm object differs across controls");
    for (const s of Object.values(servers)) await idle(s);
    report.object = {
      bytes: objects.current.bytes,
      sha256: objects.current.sha256,
    };
    for (let pair = 0; pair < 3; pair++)
      for (const concurrency of [1, 8, 32])
        for (const name of pair % 2
          ? ["control", "current"]
          : ["current", "control"])
          await hlsBatch(servers[name], name, objects[name], pair, concurrency);
  }
  report.result = "completed";
}
main()
  .catch((e) => {
    report.result = "failed";
    report.error = e.stack;
    console.error(e.stack);
    process.exitCode = 1;
  })
  .finally(async () => {
    for (const [name, s] of Object.entries(servers)) {
      await fsp
        .copyFile(s.logFile, path.join(out, `${name}-daemon.log`))
        .catch(() => {});
      await s.close(false).catch(() => {});
    }
    if (ownsOutput) await save();
    if (ownsOutput) await fsp.rm(scratch, { recursive: true, force: true });
  });
