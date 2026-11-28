"use strict";

// Exercise the shipped settings and player against deterministic subtitle
// authority. Media stays on the real Chromium video element throughout.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");
const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const root = path.resolve(__dirname, "../../crates/plurxd/src/web");
assert(process.env.WATCH_MEDIA, "Set WATCH_MEDIA to the bounded browser media fixture");
const media = fs.readFileSync(process.env.WATCH_MEDIA);
const origin = "https://translation.test";
const file = {
  id: "120", filename: "Fixture.mp4", available: true, duration_ms: 60000,
  size: media.length, video_codec: "h264", width: 640, height: 360,
  audio_streams: [{ index: 0, language: "deu", default: true }],
};
const item = { id: "20", title: "AI subtitle fixture", kind: "movie", library_id: "1",
  year: 2026, runtime_ms: 60000, watch: { position_ms: 0, watched: false } };
const operation = { id: "11111111-1111-4111-8111-111111111111", route: "audio", state: "working",
  ready_from_ms: 0, ready_to_ms: 30000, duration_ms: 60000, progress: .5 };
const literal = "A & B <b>literal</b> <00:01>\nline";

test("unified AI subtitle settings and viewer actions keep the current playback", { timeout: 45000 }, async () => {
  const executablePath = process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE;
  const browser = await chromium.launch({ headless: true, ...(executablePath ? { executablePath } : {}) });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  const errors = [], starts = [], renewals = [], releases = [], decisions = [];
  let settings = { version: 1, revision: "0", download_match: false, translate_text: false,
    from_audio: false, ahead_of_time: false, who: "everyone", quality: "balanced" };
  const readiness = { summary: { ready: 1, nodes: 1 }, nodes: [{ label: "Fixture worker", state: "ready", cores: 8 }] };
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => {
    localStorage.setItem("plurx_token", "fixture");
    localStorage.setItem("plurx_layout", "classic");
  });
  await page.route(origin + "/**", async route => {
    const request = route.request();
    const url = new URL(request.url());
    const pathname = url.pathname;
    const endpoint = pathname.replace("/api/v1", "");
    const json = value => route.fulfill({ contentType: "application/json", body: JSON.stringify(value) });
    if (pathname === "/") return route.fulfill({ contentType: "text/html", body: fs.readFileSync(path.join(root, "index.html")) });
    if (pathname.startsWith("/assets/")) {
      const asset = path.join(root, pathname.slice(8));
      return fs.existsSync(asset)
        ? route.fulfill({ contentType: pathname.endsWith(".css") ? "text/css" : "text/javascript", body: fs.readFileSync(asset) })
        : route.fulfill({ status: 404, body: "" });
    }
    if (pathname === "/fixture.mp4") {
      const range = /bytes=(\d+)-(\d*)/.exec(request.headers().range || "");
      const first = range ? Number(range[1]) : 0;
      const last = range?.[2] ? Math.min(Number(range[2]), media.length - 1) : media.length - 1;
      return route.fulfill({ status: range ? 206 : 200, contentType: "video/mp4",
        headers: { "accept-ranges": "bytes", ...(range ? { "content-range": `bytes ${first}-${last}/${media.length}` } : {}) },
        body: media.subarray(first, last + 1) });
    }
    if (endpoint === "/server") return json({ name: "Fixture", version: "dev", setup_required: false });
    if (endpoint === "/me") return json({ id: "1", username: "Fixture administrator", is_admin: true });
    if (endpoint === "/libraries") return json([{ id: "1", name: "Fixture", kind: "movie" }]);
    if (endpoint === "/ai-subtitles/preferences") return json({ language: "eng", mode: "auto" });
    if (endpoint === "/ai-subtitles/settings") {
      if (request.method() === "PUT") {
        const { expected_revision, ...update } = request.postDataJSON();
        assert.equal(expected_revision, settings.revision);
        settings = { ...update, revision: "1" };
      }
      return json({ ...settings, readiness });
    }
    if (endpoint === "/items/20") return json({ item, files: [file], children: [], ancestors: [] });
    if (endpoint.includes("/decision")) {
      decisions.push(endpoint);
      return json({ method: "direct_play", play_url: "/fixture.mp4", source: file,
        audio: file.audio_streams, subtitles: [], ladder: [], reasons: [], delivered_dynamic_range: "sdr" });
    }
    if (endpoint === "/files/120/ai-subtitles") {
      if (request.method() === "POST") {
        starts.push(request.postDataJSON());
        return json(operation);
      }
      return json({ target: "en", allowed: settings.from_audio, existing_index: null,
        route: "audio", label: "Create from the audio", alternatives: [], operation: null });
    }
    if (endpoint === `/ai-subtitles/operations/${operation.id}/viewer`) {
      if (request.method() === "DELETE") {
        releases.push(url.searchParams.get("playback_id"));
        return route.fulfill({ status: 204 });
      }
      renewals.push(request.postDataJSON());
      return json(operation);
    }
    if (endpoint === `/ai-subtitles/operations/${operation.id}/cues`) {
      const from = Number(url.searchParams.get("from_ms"));
      const to = Number(url.searchParams.get("to_ms"));
      assert.ok(to > from && to - from <= 90000);
      return json({ ...operation, cues: [{ start_ms: 0, end_ms: 18000, text: literal }] });
    }
    if (endpoint === "/dvr/overview") return json({ counts: {}, active: [], availability: "complete" });
    if (endpoint === "/dvr/reminders") return json({ rows: [] });
    return json({});
  });

  try {
    await page.goto(origin + "/#/settings/subtitles");
    await page.locator("#ai-subtitle-audio").check();
    await page.locator("#ai-subtitle-settings").getByRole("button", { name: "Save", exact: true }).click();
    await page.locator("#ai-subtitle-settings .setstate").getByText("Saved", { exact: true }).waitFor();
    assert.equal(settings.from_audio, true);
    assert.equal(settings.revision, "1");
    await page.getByText("Ready on 1 of 1 servers", { exact: true }).waitFor();

    await page.goto(origin + "/#/item/20");
    await page.locator(".btnplay").first().click();
    await page.waitForFunction(() => PLAYER?.started && document.getElementById("video")?.readyState >= 2);
    await page.evaluate(() => {
      window.captionVideo = document.getElementById("video");
      window.captionAttachment = PLAYER.mediaAttachment;
    });
    const opened = decisions.length;
    await page.locator("#player").hover();
    await page.locator("#pbsubs").click();
    await page.locator("#pmenu").getByRole("menuitem", { name: /AI subtitles/ }).click();
    await page.waitForFunction(() => document.getElementById("video")._aiCaptionTrack?.activeCues?.length > 0);
    assert.equal(await page.evaluate(() => captionVideo._aiCaptionTrack.cues[0].getCueAsHTML().textContent), literal);
    assert.equal(starts.length, 1);
    assert.equal(starts[0].route, "auto");
    assert.equal(starts[0].audio_index, 0);
    assert.equal(renewals[0].playback_id, starts[0].playback_id);
    await page.locator("#player").hover();
    await page.locator("#pbsubs").click();
    await page.locator("#pmenu").getByRole("menuitem", { name: /AI subtitles.*Stop/i }).click();
    await page.waitForFunction(() => captionVideo._aiCaptionTrack.mode === "disabled");
    assert.deepEqual(releases, [starts[0].playback_id]);
    assert.equal(await page.evaluate(() => captionVideo === document.getElementById("video") &&
      captionAttachment === PLAYER.mediaAttachment), true);
    assert.equal(decisions.length, opened);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});
