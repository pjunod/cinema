"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const fsp = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");

const lab = require("../../scripts/playback-lab");

async function withTempDir(run) {
  const directory = await fsp.mkdtemp(path.join(os.tmpdir(), "plurx-preparation-test-"));
  try {
    return await run(directory);
  } finally {
    await fsp.rm(directory, { recursive: true, force: true });
  }
}

test("measured phases stay separate from unavailable preparation facts", () => {
  const report = {
    generated_at: "2026-09-15T12:00:00.000Z",
    fatal_error: null,
    corpus: {
      "remux-h264-multitrack-1080": { size_bytes: 123_456 },
    },
    results: [{
      fixture: "remux-h264-multitrack-1080",
      quality: "auto",
      status: "passed",
      errors: [],
      playback_request_unix_ms: 1_000,
      decision: {
        method: "remux",
        delivery: { mode: "vod" },
        delivered_dynamic_range: "sdr",
      },
      start: { ttff_ms: 321 },
      server_logs: [{ ts_ms: 1_125, message: "vod session attached" }],
    }],
  };
  const preparation = {
    cache_condition: {
      fragment_index_before_pass: "absent_isolated_runtime",
      fragment_index_at_playback: "local",
      operating_system_page_cache: "uncontrolled",
    },
    source_indexing: {
      wall_status: "measured",
      wall_ms: 876,
      bytes_read_status: "unavailable",
      bytes_read: null,
    },
    shared_hydration: {
      status: "unavailable",
      wall_ms: null,
      reason: "the isolated playback-lab runtime has no artifact peer",
    },
  };

  const compact = lab.compactPreparationReport(report, preparation, {
    tested_commit: "abc123",
    host: "fixture-host",
  });

  assert.equal(compact.kind, "plurx_preparation_measurement");
  assert.equal(compact.tested_commit, "abc123");
  assert.equal(compact.host, "fixture-host");
  assert.equal(compact.fixture.source_size_bytes, 123_456);
  assert.equal(compact.recipe_identity_label, "remux/vod/sdr/auto");
  assert.deepEqual(compact.measurements.session_startup, { status: "measured", wall_ms: 125 });
  assert.deepEqual(compact.measurements.first_frame, { status: "measured", wall_ms: 321 });
  assert.equal(compact.measurements.source_indexing.bytes_read_status, "unavailable");
  assert.equal(compact.measurements.shared_hydration.status, "unavailable");
  assert.equal(compact.status, "passed");
});

test("the VOD mode writes one compact JSON artifact and no JUnit sidecar", async () => {
  await withTempDir(async (directory) => {
    const manifest = lab.loadManifest();
    const fixture = manifest.fixtures.find((entry) => entry.id === "remux-h264-multitrack-1080");
    const json = path.join(directory, "preparation.json");
    const state = { server_closed: 0, driver_closed: 0 };
    let playbackRequestAt = 0;
    const server = {
      baseUrl: "http://127.0.0.1:41001",
      runtime: "/tmp/playback-lab-preparation-runtime",
      token: "contract-token",
      files: new Map([[fixture.filename, { id: 7, filename: fixture.filename }]]),
      preparation: {
        source_indexing: {
          wall_status: "measured",
          wall_ms: 400,
          bytes_read_status: "unavailable",
          bytes_read: null,
        },
        shared_hydration: { status: "unavailable", wall_ms: null, reason: "single node" },
      },
      close: async () => { state.server_closed += 1; },
    };
    const driver = {
      start: async () => {},
      close: async () => { state.driver_closed += 1; },
      exec: async () => true,
    };
    const dependencies = {
      buildFixtures: async () => ({
        directory: "/tmp/playback-lab-preparation-fixtures",
        metadata: { [fixture.id]: { size_bytes: 99_001 } },
      }),
      startServer: async (options) => {
        assert.equal(options.enable_vod, true);
        return server;
      },
      createDriver: () => driver,
      prepareBrowser: async () => ({
        browser: "Contract Browser",
        caps: { vcodec: "h264", acodec: "aac", hdr: 0 },
        native_hls: false,
      }),
      api: async (_base, route) => route.startsWith("/system/logs")
        ? [{ ts_ms: playbackRequestAt + 150, level: "INFO", message: "vod session attached" }]
        : { version: "contract" },
      runOneCase: async (_driver, _server, _manifest, _fixture, testCase) => {
        playbackRequestAt = Date.now();
        return {
          name: testCase.name,
          fixture: testCase.fixture,
          quality: testCase.quality,
          operation: testCase.operation,
          status: "passed",
          duration_ms: 2_000,
          playback_request_unix_ms: playbackRequestAt,
          errors: [],
          warnings: [],
          decision: { method: "remux", delivery: { mode: "vod" } },
          start: { ttff_ms: 275 },
          metrics: {
            decision_method: "remux",
            actual_method: "remux",
            copy_hls: true,
            ttff_ms: 275,
            clock_rate: 1,
            hitches: 0,
            stalls: 0,
          },
        };
      },
    };

    const outcome = await lab.executeRun(manifest, {
      suite: "vod",
      case: "steady",
      preparation_measurement: true,
      json,
    }, dependencies);

    assert.deepEqual(outcome, { code: 0, error: null });
    const artifact = JSON.parse(await fsp.readFile(json, "utf8"));
    assert.equal(artifact.kind, "plurx_preparation_measurement");
    assert.equal(artifact.fixture.source_size_bytes, 99_001);
    assert.deepEqual(artifact.measurements.session_startup, { status: "measured", wall_ms: 150 });
    assert.equal(artifact.measurements.shared_hydration.status, "unavailable");
    assert.deepEqual(await fsp.readdir(directory), ["preparation.json"]);
    assert.deepEqual(state, { server_closed: 1, driver_closed: 1 });
  });
});
