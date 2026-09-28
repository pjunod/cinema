"use strict";

(function publish(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.PlurxLiveTv = api;
})(typeof self !== "undefined" ? self : globalThis, function buildLiveTv() {
  // Every copy key a start answer can render. A code with no row here is a code
  // this client has never heard of, and the honest thing to say about it is
  // that the owner could not serve the channel — which is also what the shared
  // fixture rules for `node_maintenance`.
  const ERROR_COPY = {
    live_tv_disabled: ["Live TV is off", "An administrator can enable it in Settings → Developer."],
    live_tv_protocol_unready: ["Live TV route is updating", "This tuner owner cannot serve the requested route yet. Another compatible owner may still work."],
    owner_unavailable: ["Tuner owner unavailable", "The selected tuner owner cannot serve Live TV right now."],
    no_answer: ["The server did not answer", "Press the channel again."],
    invalid_request: ["Live TV could not read that request", "Reload the page and choose the channel again."],
    admin_required: ["Live TV needs an administrator", "This account cannot change Live TV on this server."],
    invalid_settings: ["Live TV settings are incomplete", "An administrator can finish the tuner setup in Settings → Developer."],
    tuner_capacity: ["All Live TV slots are busy", "Close another Live TV session or try this channel again shortly."],
    tuner_unavailable: ["The tuner could not start this channel", "A tuner, signal, or channel authorization may be unavailable."],
    channel_not_found: ["Channel no longer available", "Reload the channel list and choose another channel."],
    drm_unsupported: ["Protected channel", "plurx does not play DRM-protected television."],
    codec_unsupported: ["Channel format unsupported", "The tuner owner cannot decode this channel into the compatible live format."],
    startup_timeout: ["Channel took too long to start", "No first live segment arrived before the startup deadline."],
    stream_failed: ["Live stream stopped", "The tuner or transcoder stopped producing live television."],
    source_format_changed: ["Broadcast format changed", "The player will release this session and select a fresh compatible route once."],
    capability_expired: ["Live session expired", "The player was idle or disconnected. Start the channel again."],
    settings_conflict: ["Live TV settings changed", "Reload the channel list before starting another channel."],
    guide_unavailable: ["No programme guide yet", "The tuner owner has not fetched a guide. Channels still play; rows show number and callsign only."],
  };

  // The four typed refusals the ingress decides before a request ever reaches a
  // tuner owner. They cannot have left a start behind, so their hint goes; and
  // no button can make any of them succeed, so no retry is offered.
  const INGRESS_REFUSALS = new Set([
    "invalid_request",
    "admin_required",
    "invalid_settings",
    "live_tv_disabled",
  ]);

  // The one reducer, ruled by tests/playback/live-tv-start-cases.json and
  // shared in shape with the Apple and Android leases. It is a pure function of
  // the answer — `{status, body}` for anything that arrived, anything else for
  // a request that did not come back — and it never refuses to start.
  //
  // A hint is kept unless the body is typed with `owner_decided: true`, or is a
  // typed refusal the ingress made on its own. Everything else leaves a start
  // that may or may not exist on an owner, and the hint is the only handle for
  // retiring it later.
  function startOutcome(answer) {
    const body = (answer && answer.body) || {};
    const code = typeof body.code === "string" && body.code ? body.code : null;
    // No typed body is no answer: the client says so and the next press
    // repeats. It never invents a quarantine the server did not ask for.
    if (!code) return { render: "no_answer", offerRetry: true, keepHint: true, replay: true };
    // The status is half of what makes a refusal the ingress's own. A 4xx is
    // the ingress rejecting the request itself, before any owner saw it, so
    // there is no start to retire and the handle goes. The same code inside a
    // 5xx is a failure on the way to — or at — an owner that may already have
    // opened a tuner, and the handle is the only thing that could retire it.
    const status = Number(answer && answer.status);
    const ingress = INGRESS_REFUSALS.has(code) && status >= 400 && status < 500;
    const retry = typeof body.retry === "string" ? body.retry : null;
    return {
      render: Object.prototype.hasOwnProperty.call(ERROR_COPY, code) ? code : "owner_unavailable",
      offerRetry: retry !== "never" && !ingress,
      keepHint: !(body.owner_decided === true || ingress),
      replay: false,
    };
  }

  function watchableOffers(error) {
    const rows = error && error.code === "tuner_capacity" && error.answer && error.answer.body
      ? error.answer.body.watchable : null;
    if (!Array.isArray(rows)) return [];
    const seen = new Set();
    return rows.filter(row => row && typeof row.channel_id === "string"
        && typeof row.guide_number === "string" && row.guide_number.trim()
        && !seen.has(row.channel_id) && (seen.add(row.channel_id), true))
      .map(row => ({ channelId: row.channel_id, label: `Watch ${row.guide_number} instead` }));
  }

  function errorView(error) {
    const code = String((error && error.code) || "owner_unavailable");
    const outcome = startOutcome({
      status: error && error.status,
      body: { code, retry: error && error.retry, owner_decided: error && error.owner_decided },
    });
    const selected = ERROR_COPY[outcome.render] || ERROR_COPY.owner_unavailable;
    return { code, title: selected[0], detail: selected[1], retryable: outcome.offerRetry, offers: watchableOffers(error) };
  }

  function channelView(channel) {
    // Allowlist the one playable state. A denylist of known-protected shapes
    // fails open: a lineup that grows a new `support` value renders an enabled
    // "Watch live" for a channel this client must refuse.
    const playable = !!channel && channel.drm !== true && channel.support === "ready";
    const protectedChannel = !playable;
    return {
      disabled: protectedChannel,
      label: protectedChannel ? "Protected · unsupported" : "Available",
      tone: protectedChannel ? "warn" : "ready",
    };
  }

  function measuredSourceFormat(channel, nowSeconds = Math.floor(Date.now() / 1000), programmeEnd = null) {
    const raw = channel && channel.source_format;
    if (!raw || typeof raw !== "object" || !Number.isSafeInteger(raw.observed_at) || raw.observed_at <= 0) return null;
    const expiry = Math.min(raw.observed_at + 20 * 60,
      Number.isInteger(programmeEnd) ? programmeEnd : Number.MAX_SAFE_INTEGER);
    if (Number.isFinite(nowSeconds) && nowSeconds >= expiry) return null;
    const dimension = value => Number.isInteger(value) && value >= 1 && value <= 16384 ? value : null;
    const channels = Number.isInteger(raw.audio_channels) && raw.audio_channels >= 1 && raw.audio_channels <= 32
      ? raw.audio_channels : null;
    const scan = raw.scan === "progressive" || raw.scan === "interlaced" ? raw.scan : null;
    const layout = typeof raw.audio_layout === "string" && /^[a-z0-9._+ -]{1,32}$/.test(raw.audio_layout)
      ? raw.audio_layout.trim() || null : null;
    return {
      video_width: dimension(raw.video_width),
      video_height: dimension(raw.video_height),
      scan,
      audio_channels: channels,
      audio_layout: layout,
      observed_at: raw.observed_at,
    };
  }

  function pictureClass(channel, nowSeconds, programmeEnd) {
    const height = measuredSourceFormat(channel, nowSeconds, programmeEnd)?.video_height;
    if (height > 2160) return "4K+";
    if (height === 2160) return "4K";
    if (height >= 720) return "HD";
    if (height > 0) return "SD";
    if (channel && channel.hd === true) return "HD";
    if (channel && channel.hd === false) return "SD";
    return null;
  }

  function audioDescription(channel, nowSeconds, programmeEnd) {
    const format = measuredSourceFormat(channel, nowSeconds, programmeEnd);
    const codec = typeof channel?.audio_codec === "string" ? channel.audio_codec.trim().toUpperCase() : "";
    const layout = format?.audio_layout;
    const layoutLabel = layout === "mono" ? "Mono" : layout === "stereo" ? "Stereo" : layout;
    const count = !layoutLabel && format?.audio_channels ? `${format.audio_channels} ch` : "";
    return [codec, layoutLabel || count].filter(Boolean).join(" ");
  }

  function sourceDetails(channel, nowSeconds, programmeEnd) {
    if (!channel) return { compact: [], exact: [], observedAt: null };
    const format = measuredSourceFormat(channel, nowSeconds, programmeEnd);
    const picture = pictureClass(channel, nowSeconds, programmeEnd);
    const video = typeof channel.video_codec === "string" ? channel.video_codec.trim().toUpperCase() : "";
    const audio = audioDescription(channel, nowSeconds, programmeEnd);
    const scan = format?.scan === "progressive" ? "p" : format?.scan === "interlaced" ? "i" : "";
    const dimensions = format?.video_width && format?.video_height
      ? `${format.video_width}×${format.video_height}${scan}` : "";
    return {
      compact: [picture, video, audio].filter(Boolean),
      exact: [dimensions || picture, video, audio].filter(Boolean),
      observedAt: format?.observed_at || null,
    };
  }

  // Information facts are normalized from one delivery object. In particular,
  // browser intrinsic dimensions are a presentation observation, never an
  // encoded output measurement or a fallback for a missing planned width.
  function pictureDimension(value) {
    return Number.isSafeInteger(value) && value > 0 && value <= 16384 ? value : null;
  }

  function pictureFrame(value) {
    return { width: pictureDimension(value?.width ?? value?.video_width),
      height: pictureDimension(value?.height ?? value?.video_height) };
  }

  function pictureGcd(a, b) {
    while (b) [a, b] = [b, a % b];
    return a;
  }

  function parsePictureRatio(value) {
    if (typeof value !== "string" || !/^[1-9][0-9]*:[1-9][0-9]*$/.test(value.trim())) return null;
    const [numerator, denominator] = value.trim().split(":").map(Number);
    if (!Number.isSafeInteger(numerator) || !Number.isSafeInteger(denominator)) return null;
    const divisor = pictureGcd(numerator, denominator);
    return { numerator: numerator / divisor, denominator: denominator / divisor };
  }

  function pictureFrameText(frame) {
    if (frame.width && frame.height) return `${frame.width}×${frame.height}`;
    if (frame.height) return `Height ${frame.height}`;
    return "Unavailable";
  }

  function pictureDar(frame, sar) {
    if (!frame.width || !frame.height || !sar) return null;
    const numerator = frame.width * sar.numerator;
    const denominator = frame.height * sar.denominator;
    if (!Number.isSafeInteger(numerator) || !Number.isSafeInteger(denominator)) return null;
    const divisor = pictureGcd(numerator, denominator);
    return { numerator: numerator / divisor, denominator: denominator / divisor };
  }

  function pictureRatioText(ratio) {
    return ratio ? `${ratio.numerator}:${ratio.denominator}` : "Unavailable";
  }

  function pictureFrameComparison(source, stream, provenance) {
    if (!source.width || !source.height || !stream.width || !stream.height) return "Unavailable";
    const dw = stream.width - source.width, dh = stream.height - source.height;
    const planned = provenance === "server_plan";
    if (!dw && !dh) return planned ? "No resize planned" : "Frame dimensions unchanged";
    if (dw <= 0 && dh <= 0) return planned ? "Resolution reduction planned" : "Stream resolution reduced";
    if (dw >= 0 && dh >= 0) return planned ? "Larger frame dimensions planned" : "Stream frame dimensions increased";
    return planned ? "Frame dimensions change planned" : "Stream frame dimensions changed";
  }

  function pictureDisplayAgreement(frame, dar, display) {
    if (!dar || !display.width || !display.height) return null;
    const expectedWidth = display.height * dar.numerator / dar.denominator;
    const expectedHeight = display.width * dar.denominator / dar.numerator;
    return (display.width >= Math.floor(expectedWidth) && display.width <= Math.ceil(expectedWidth)) ||
      (display.height >= Math.floor(expectedHeight) && display.height <= Math.ceil(expectedHeight));
  }

  function normalizeLiveTvPictureFacts({ delivery, presentation, channelObservation,
      attachmentCurrent, nowSeconds, compatibleAperture = false }) {
    const plan = attachmentCurrent ? delivery : null;
    const observed = !plan && channelObservation &&
      Number.isSafeInteger(channelObservation.observed_at) &&
      nowSeconds < channelObservation.observed_at + 20 * 60 ? channelObservation : null;
    const source = pictureFrame(plan?.source || observed);
    const stream = pictureFrame(plan?.output);
    const display = attachmentCurrent ? pictureFrame(presentation) : pictureFrame(null);
    const sourceSar = parsePictureRatio(plan?.source?.sample_aspect_ratio);
    const sourceDar = pictureDar(source, sourceSar);
    return {
      source, stream, display, sourceSar, sourceDar,
      sourceProvenance: plan?.source ? "source_probe" : observed ? "channel_observation" : "unavailable",
      streamProvenance: plan?.output ? "server_plan" : "unavailable",
      displayProvenance: display.width && display.height ? "player_presentation" : "unavailable",
      frameComparison: plan?.source ? pictureFrameComparison(source, stream, "server_plan") : "Unavailable",
      aspectComparison: compatibleAperture && plan?.source &&
        pictureDisplayAgreement(source, sourceDar, display) === true
        ? "Player display is consistent with source shape" : "Not verified",
    };
  }

  function formatLiveTvPictureFacts(facts) {
    return {
      source_resolution: pictureFrameText(facts.source),
      source_resolution_note: facts.sourceProvenance === "source_probe" ? "Source probe" :
        facts.sourceProvenance === "channel_observation" ? "Last observed broadcast" : "Unavailable",
      source_pixel_aspect: pictureRatioText(facts.sourceSar),
      source_display_aspect: pictureRatioText(facts.sourceDar),
      stream_frame: pictureFrameText(facts.stream),
      stream_frame_note: facts.streamProvenance === "server_plan" ? "Planned output" : "Unavailable",
      stream_pixel_aspect: "Not measured",
      decode_resolution: pictureFrameText(facts.display),
      decode_resolution_note: facts.displayProvenance === "player_presentation"
        ? "Browser intrinsic dimensions" : "Unavailable",
      frame_comparison: facts.frameComparison,
      aspect_comparison: facts.aspectComparison,
    };
  }

  function liveTvReasonText(reasons) {
    if (!Array.isArray(reasons)) return "The server did not provide a conversion reason.";
    const seen = new Set();
    const explanations = [];
    for (const reason of reasons) {
      const explanation = typeof reason?.explanation === "string" ? reason.explanation.trim() : "";
      if (!explanation) continue;
      const pair = `${String(reason.code || "")}\u0000${explanation}`;
      if (!seen.has(pair)) { seen.add(pair); explanations.push(explanation); }
    }
    return explanations.join(" · ") || "The server did not provide a conversion reason.";
  }

  // Source facts from the tuner and the bounded FFmpeg input description.
  // Missing fields stay missing; codecs never imply resolution or layout.
  function channelBadges(channel, nowSeconds, programmeEnd) {
    const badges = sourceDetails(channel, nowSeconds, programmeEnd).compact;
    return badges.filter((badge, index) => badges.indexOf(badge) === index);
  }

  function capability(info) {
    return info && typeof info.session_id === "string" && info.session_id ? info.session_id : null;
  }

  // A hint is `{request_id, touched_at}` and nothing else. Only random request
  // identities are persisted, never capabilities, account tokens, channel IDs
  // or tuner URLs — the hint is a handle for retiring or resuming a start on
  // the owner, and it is worthless to anyone who steals it.
  //
  // Nothing here ever throws and nothing here ever refuses: a browser that
  // will not keep a hint loses the ability to tidy up after itself, which is
  // never a reason to stop the viewer watching television.
  const HINT_PREFIX = "plurx_live_tv_hint_v1:";
  // Hints are forgotten on release, on a retire and on an owner's verdict, so
  // the store is normally one row deep. The cap only bounds the pathological
  // case — a browser that never manages a clean DELETE — by dropping the
  // oldest handle, never by refusing to mint a new one.
  const HINT_LIMIT = 32;

  class StartHints {
    constructor(storage, now) {
      this.storage = storage;
      this.now = now;
      this.prefix = HINT_PREFIX;
    }

    // Every persisted hint, oldest touch first.
    list() {
      const out = [];
      try {
        const storage = this.storage();
        for (let i = 0; i < storage.length; i++) {
          const key = storage.key(i);
          if (!key || !key.startsWith(this.prefix)) continue;
          const touchedAt = Number(storage.getItem(key));
          out.push({ id: key.slice(this.prefix.length), touchedAt: Number.isFinite(touchedAt) ? touchedAt : 0 });
        }
      } catch (_) { return []; }
      return out.sort((a, b) => a.touchedAt - b.touchedAt);
    }

    remember(id) {
      if (!id) return;
      const existing = this.list();
      for (let i = 0; i <= existing.length - HINT_LIMIT; i++) this.forget(existing[i].id);
      this.touch(id);
    }

    touch(id) {
      if (!id) return;
      try { this.storage().setItem(this.prefix + id, String(this.now())); } catch (_) { /* best effort */ }
    }

    forget(id) {
      if (!id) return;
      try { this.storage().removeItem(this.prefix + id); } catch (_) { /* best effort */ }
    }
  }

  // Serializes channel changes around a single owner-bound capability. A new
  // selection never opens until the preceding start has either failed or been
  // released. A failed DELETE retains ownership: subsequent selections must
  // retry cleanup before opening another tuner. Memory is one capability, not
  // an ever-growing history of every channel watched.
  class Lease {
    constructor(requests) {
      if (!requests || typeof requests.start !== "function" || typeof requests.release !== "function") {
        throw new TypeError("Live TV requires start and release requests");
      }
      this.requests = requests;
      this.generation = 0;
      this.current = null;
      // The generation that owns `current`. `stop()` bumps `this.generation`
      // synchronously but only sets `releasing` two microtasks later, so a
      // renewing request issued in that window would otherwise pass a
      // generation check that compares the bumped value against itself.
      this.currentGeneration = 0;
      this.releasing = false;
      this.tail = Promise.resolve();
    }

    start(channelId) {
      const generation = ++this.generation;
      const operation = this.tail.catch(() => {}).then(async () => {
        await this.releaseCurrent();
        if (generation !== this.generation) return null;
        const info = await this.requests.start(channelId);
        if (!capability(info)) throw new Error("Live TV returned an invalid session");
        this.current = info;
        this.currentGeneration = generation;
        if (generation !== this.generation) {
          await this.releaseCurrent();
          return null;
        }
        return info;
      });
      this.tail = operation;
      return operation;
    }

    // A session the owner handed back on resume enters through exactly the
    // path a fresh start uses: same generation bump, same release of whatever
    // was current, same ownership record. The only difference is that the
    // capability came from `/starts/{id}/resume` instead of a POST.
    adopt(info) {
      const generation = ++this.generation;
      const operation = this.tail.catch(() => {}).then(async () => {
        await this.releaseCurrent();
        if (generation !== this.generation || !capability(info)) return null;
        this.current = info;
        this.currentGeneration = generation;
        return info;
      });
      this.tail = operation;
      return operation;
    }

    stop() {
      ++this.generation;
      const operation = this.tail.catch(() => {}).then(() => this.releaseCurrent());
      this.tail = operation;
      return operation;
    }

    async releaseCurrent() {
      const info = this.current;
      const id = capability(info);
      if (!id) return false;
      this.releasing = true;
      try {
        await this.requests.release(id);
        if (this.current === info) this.current = null;
        return true;
      } finally { this.releasing = false; }
    }

    async keepalive() {
      const generation = this.generation;
      const info = this.current;
      const id = capability(info);
      if (!id || this.releasing || this.currentGeneration !== this.generation) return null;
      if (typeof this.requests.keepalive !== "function") return null;
      const result = await this.requests.keepalive(id);
      return generation === this.generation && this.current === info ? result : null;
    }

    async status() {
      const generation = this.generation;
      const info = this.current;
      const id = capability(info);
      // A status poll renews the server lease exactly as keepalive does, so it
      // must observe the same guards: polling a capability whose DELETE is
      // already in flight — or one a newer generation has superseded — renews
      // the very lease this client is dropping.
      if (!id || this.releasing || this.currentGeneration !== this.generation) return null;
      if (typeof this.requests.status !== "function") return null;
      const result = await this.requests.status(id);
      return generation === this.generation && this.current === info ? result : null;
    }
  }


  // ---- programme guide: the pure half -------------------------------------
  // Every one of these is a total function over a guide document and answers
  // the same way on the web, on Apple and on Android — the cases they must
  // reproduce are tests/playback/live-tv-guide-cases.json. Keeping them here
  // rather than in index.html is what lets a test call them directly.

  // When to ask for the guide again, in milliseconds, from the answer the
  // owner just gave. The owner's clock decides: `next_refresh_at` is when it
  // expects to have something new, and the client asks a few seconds after
  // that rather than a few seconds before. `guide_poll_min_s` is a floor on
  // fan-out and applies whatever the owner said; an `unavailable` guide that
  // names no next refresh is asked for again on the short unavailable cadence,
  // because an owner with nothing to serve is usually about to have something.
  // Every number comes from the shared contract table — none is written here.
  function guidePollDelayMs(guide, nowSeconds, timings) {
    const floor = timings.guide_poll_min_s;
    const at = guide && Number.isFinite(guide.next_refresh_at) ? guide.next_refresh_at : null;
    if (at !== null) {
      const delay = Math.max(at + timings.guide_poll_after_next_refresh_s, nowSeconds + floor) * 1000
        - nowSeconds * 1000;
      // An owner that says it will refresh next week must not park the grid
      // until then: a channel line-up changes, a guide host comes back, and a
      // page left open has to notice. The ceiling bounds only this branch —
      // the other two return the contract's own cadences. The floor still
      // wins if the two were ever set to cross, because a fan-out floor is a
      // promise to the owner and a ceiling is only a promise to the viewer.
      const ceiling = Number.isFinite(timings.guide_poll_ceiling_s)
        ? timings.guide_poll_ceiling_s * 1000 : Infinity;
      return Math.max(floor * 1000, Math.min(delay, ceiling));
    }
    // A read that failed is paced like an `unavailable` answer: no document at
    // all and a document with nothing in it carry the same information — the
    // owner has no guide to serve yet — and the three clients pace them alike.
    if (!guide || guide.freshness === "unavailable") return timings.guide_poll_unavailable_s * 1000;
    return floor * 1000;
  }

  function guideChannel(guide, channelId) {
    if (!guide || !Array.isArray(guide.channels)) return null;
    return guide.channels.find(channel => channel && channel.id === channelId) || null;
  }

  // (guide, channel id, now) → { now, next, progress }.
  // The start instant belongs to the programme that starts; the end instant
  // does not. Without that rule a viewer at exactly 8:30 sees two programmes
  // on air, and "what is on now" stops being a question with one answer.
  function programmeAt(guide, channelId, now) {
    const channel = guideChannel(guide, channelId);
    const rows = channel && Array.isArray(channel.programmes) ? channel.programmes : [];
    let current = null, next = null;
    for (const row of rows) {
      if (!row || typeof row.start !== "number" || typeof row.end !== "number") continue;
      if (row.start <= now && now < row.end) { current = row; continue; }
      if (row.start > now && (next === null || row.start < next.start)) next = row;
    }
    // A real hole in the guide is a state to render, not one to paper over
    // with the programme that just ended.
    const span = current ? current.end - current.start : 0;
    const progress = current && span > 0 ? (now - current.start) / span : null;
    return { now: current, next, progress };
  }

  // Positioned cells for the half-hour grid. Clips to the window, never
  // overlaps, and reports where the red now line goes. Geometry only: the
  // caller decides what a cell looks like.
  function gridLayout(guide, channels, window, now, slotSeconds, pxPerSlot) {
    const slot = slotSeconds > 0 ? slotSeconds : 1800;
    const px = pxPerSlot > 0 ? pxPerSlot : 240;
    const scale = seconds => ((seconds - window.start) / slot) * px;
    const rows = (channels || []).map(channel => {
      const source = guideChannel(guide, channel.id);
      const programmes = source && Array.isArray(source.programmes) ? source.programmes : [];
      const cells = [];
      for (const row of programmes) {
        // Total over a malformed guide, exactly as programmeAt is. The guide
        // host is untrusted, and one bad row used to take the whole grid
        // render down with a TypeError.
        if (!row || typeof row.start !== "number" || typeof row.end !== "number") continue;
        const start = Math.max(row.start, window.start);
        const end = Math.min(row.end, window.end);
        if (!(end > start)) continue;
        cells.push({
          programme: row,
          left: scale(start),
          width: scale(end) - scale(start),
          airing: row.start <= now && now < row.end,
          clipped: row.start < window.start || row.end > window.end,
        });
      }
      return { channel, cells };
    });
    return {
      rows,
      totalWidth: scale(window.end),
      nowX: now >= window.start && now <= window.end ? scale(now) : null,
      slots: Math.max(1, Math.round((window.end - window.start) / slot)),
    };
  }

  // Search matches the number, the callsign, and what is on now — typing what
  // you can see on screen should find the channel showing it.
  function filterChannels(channels, guide, opts, now) {
    const options = opts || {};
    const query = String(options.query || "").trim().toLocaleLowerCase();
    const favorites = options.filter === "favorites";
    const hideProtected = options.hideProtected === true;
    return (channels || []).filter(channel => {
      if (!channel) return false;
      if (favorites && !channel.favorite) return false;
      if (hideProtected && channelView(channel).disabled) return false;
      if (!query) return true;
      const airing = programmeAt(guide, channel.id, now || 0).now;
      const haystack = `${channel.guide_number} ${channel.guide_name} ${airing ? airing.title : ""}`;
      return haystack.toLocaleLowerCase().includes(query);
    });
  }

  // Which channel is ±1 from `current` in the visible order, wrapping. An
  // unknown current lands on the first, so channel-up from a channel that was
  // just filtered away still goes somewhere.
  function adjacentChannel(visible, currentId, delta) {
    const list = visible || [];
    if (list.length === 0) return null;
    const ids = list.map(entry => (typeof entry === "string" ? entry : entry.id));
    const at = ids.indexOf(currentId);
    if (at === -1) return ids[0];
    const step = Number(delta) || 0;
    const next = ((at + step) % ids.length + ids.length) % ids.length;
    return ids[next];
  }

  // Half-hour column headings across the window, for the grid's sticky header.
  function gridSlots(window, slotSeconds) {
    const slot = slotSeconds > 0 ? slotSeconds : 1800;
    const out = [];
    for (let at = window.start; at < window.end; at += slot) out.push(at);
    return out;
  }

  // Where the guide's data stops on a channel — the hatched "guide data ends"
  // cell. Null when it runs past the window, which is the ordinary case.
  function guideEnds(guide, channelId, window) {
    const channel = guideChannel(guide, channelId);
    const rows = channel && channel.programmes ? channel.programmes : [];
    // No rows is not "the guide ends here" — it is a channel the guide says
    // nothing about, which happens for every channel when the source is off.
    // Answering window.start drew "Guide data ends 3:00 PM" on every row of an
    // empty grid, which is a statement the page had no basis for.
    if (rows.length === 0) return null;
    // Take the furthest end rather than the last row's: the contract sorts
    // programmes by start, which does not make the final end the maximum, and
    // a feed that is out of order should not shorten the guide.
    let end = null;
    for (const row of rows) {
      if (!row || typeof row.end !== "number") continue;
      if (end === null || row.end > end) end = row.end;
    }
    if (end === null) return null;
    return window && end >= window.end ? null : end;
  }

  // ---- recording: the pure half -------------------------------------------
  // The DVR answers a guide page with three documents — a schedule, the
  // caller's reminders and a status — read once per guide load and again after
  // every mutation, never polled. Everything below turns those three into the
  // marks, sentences and countdowns the renderers print, so a test can ask for
  // each answer without a DOM and without a server.

  /// An airing with less than this left is not worth a tuner, a file and a
  /// library item. `plurx_core::dvr::DVR_MIN_USEFUL_S`, and the reason the
  /// routes answer `airing_past`: an action that can only be refused is worse
  /// than an absent one, so the cell does not offer it.
  const DVR_MIN_USEFUL_S = 60;

  // An airing is `(channel_id, airing_start)` for its whole life — one unique
  // index covers every state — so that pair, and never the row id, is what a
  // guide cell looks itself up by.
  function dvrAiringKey(channelId, airingStart) {
    return `${channelId} ${airingStart}`;
  }

  // One lookup for the whole grid, built once per render. A fortnight of rules
  // against a full lineup is thousands of cells, and asking each of them to
  // scan both arrays is the difference between a paint and a stall.
  function dvrIndex(schedule, reminders) {
    const rows = new Map(), bells = new Map();
    for (const row of (schedule && schedule.rows) || []) {
      if (!row || typeof row.airing_start !== "number") continue;
      rows.set(dvrAiringKey(row.channel_id, row.airing_start), row);
    }
    for (const bell of reminders || []) {
      // `acked`, `expired` and `moved` are history. Only a reminder that will
      // still fire, or has just fired, earns a bell on the cell.
      if (!bell || typeof bell.airing_start !== "number") continue;
      if (bell.state !== "armed" && bell.state !== "fired") continue;
      bells.set(dvrAiringKey(bell.channel_id, bell.airing_start), bell);
    }
    return {
      row: (channelId, start) => rows.get(dvrAiringKey(channelId, start)) || null,
      reminder: (channelId, start) => bells.get(dvrAiringKey(channelId, start)) || null,
    };
  }

  // 0..1 through the capture, which is the padded span and not the airing:
  // the progress underline has to reach its end when the file closes, not when
  // the programme does.
  function dvrCaptureProgress(row, now) {
    if (!row) return 0;
    const span = row.capture_end - row.capture_start;
    if (!(span > 0)) return 0;
    return Math.min(1, Math.max(0, (now - row.capture_start) / span));
  }

  // What one cell wears: at most two marks, because there are at most two
  // facts — what the DVR will do with this airing, and whether the viewer
  // asked to be told when it starts.
  //
  // A `cancelled` row earns no mark at all. `?cancelled=1` keeps those rows in
  // the schedule so they can be restored from the Scheduled list, and an
  // airing nobody is recording must not look scheduled in the guide.
  function dvrMarks(row, reminder, now) {
    const marks = [];
    const state = row && row.state;
    if (state === "recording") {
      marks.push({ kind: "recording", shape: "rec", label: "Recording now",
        progress: dvrCaptureProgress(row, now) });
    } else if (state === "conflict") {
      marks.push({ kind: "conflict", shape: "dot",
        label: (row && row.state_reason) || "No tuner is free for this" });
    } else if (state === "withdrawn" || state === "stale") {
      marks.push({ kind: state, shape: "hollow",
        label: state === "withdrawn" ? "No enabled rule matches this any more"
          : "The programme moved or was renamed" });
    } else if (state === "scheduled") {
      // Two dots for a rule's episode, one for a person's decision: the two
      // are taken away by different things, so they cannot look the same.
      marks.push(row.rule_id
        ? { kind: "series", shape: "dots", label: "Scheduled by a series rule" }
        : { kind: "once", shape: "dot", label: "Scheduled" });
    }
    if (reminder) marks.push({ kind: "reminder", shape: "bell", label: "Reminder set" });
    return marks;
  }

  // What a guide cell may offer, given when the programme is on and what the
  // DVR already knows about it. Every "no" here is a request the server would
  // refuse: `airing_past` for anything finished or already started, and a
  // second Record for an airing that is already on the schedule.
  function dvrAiringActions(programme, row, reminder, now) {
    const start = programme && Number(programme.start);
    const end = programme && Number(programme.end);
    if (!Number.isFinite(start) || !Number.isFinite(end)) {
      return { watch: null, record: null, series: false, remind: null };
    }
    const onAir = start <= now && now < end;
    const finished = end - now < DVR_MIN_USEFUL_S;
    const state = row && row.state;
    const pending = state === "scheduled" || state === "conflict"
      || state === "withdrawn" || state === "stale";
    return {
      // A future cell cannot be tuned, so Watch becomes the promise to be
      // shown it at its start — a reminder with no lead at all.
      watch: finished ? null : onAir ? "now" : "at",
      record: finished ? null : state === "recording" ? "stop" : pending ? "skip" : "record",
      series: !finished && !!(programme && programme.title),
      // A reminder for a programme that has already started is a reminder
      // about the past; the route answers `airing_past`.
      remind: finished || onAir ? null : reminder ? "clear" : "set",
    };
  }

  // "2 of 4 tuners · 1 reserved for viewing".
  //
  // Counted from the schedule rather than from `slots.recording` because the
  // schedule is refetched after every mutation and the status is not: a viewer
  // who has just pressed Stop must not read a sentence that still describes
  // the tuners before they pressed it.
  function dvrTunerLine(status, schedule) {
    const slots = (status && status.slots) || null;
    const max = Number(slots && slots.max) || 0;
    if (!(max > 0)) return "";
    const rows = schedule && Array.isArray(schedule.rows) ? schedule.rows : null;
    const busy = rows
      ? rows.filter(row => row && row.state === "recording").length
      : Number(slots.recording) || 0;
    const reserve = Math.max(0, Number(slots.reserve) || 0);
    const head = `${busy} of ${max} tuner${max === 1 ? "" : "s"}`;
    return reserve ? `${head} · ${reserve} reserved for viewing` : head;
  }

  // How long until an airing starts, in the words the reminder overlay uses.
  //
  // Deliberately coarse above a minute: the overlay repaints on the same 30 s
  // tick that asks for due reminders, and a seconds counter that only moves
  // twice a minute reads as broken rather than as precise.
  function dvrCountdown(seconds) {
    const left = Math.round(Number(seconds) || 0);
    if (left <= 0) return "now";
    if (left < 60) return `in ${left}s`;
    if (left < 3600) return `in ${Math.floor(left / 60)} min`;
    const hours = Math.floor(left / 3600), minutes = Math.floor((left % 3600) / 60);
    return minutes ? `in ${hours}h ${minutes}m` : `in ${hours}h`;
  }

  // The overlay's own window. A reminder fires `lead_s` before the start, so
  // the bar drains across exactly that span and the overlay retires itself at
  // zero rather than waiting for a poll to tell it the moment passed.
  function dvrReminderCountdown(reminder, now) {
    const start = Number(reminder && reminder.airing_start) || 0;
    // Zero lead is legal — "Watch at 8:00" sets one — and would divide by it.
    const lead = Math.max(1, Number(reminder && reminder.lead_s) || 0);
    const left = start - now;
    return {
      seconds: left,
      label: dvrCountdown(left),
      // 1 at the instant it fired, 0 at the start.
      fraction: Math.min(1, Math.max(0, left / lead)),
      expired: left <= 0,
    };
  }

  // One running capture, for the row beside Activity's Stop. The pill at the
  // top of every page prints the server's own sentence from `/activity`; this
  // sentence exists because that list carries no recording id, so the page has
  // to read the rows themselves to be able to stop one.
  function dvrRecordingDetail(row, now) {
    if (!row) return "";
    const parts = [`${row.guide_number} ${row.channel_name}`.trim()];
    const left = Math.max(0, Math.round((Number(row.capture_end) - now) / 60));
    parts.push(`${left} min left`);
    if (Number(row.bytes) > 0) parts.push(`${(Number(row.bytes) / 1e9).toFixed(1)} GB`);
    return parts.join(" · ");
  }

  return Object.freeze({
    Lease,
    StartHints,
    startOutcome,
    guidePollDelayMs,
    channelView,
    channelBadges,
    measuredSourceFormat,
    sourceDetails,
    parsePictureRatio,
    normalizeLiveTvPictureFacts,
    formatLiveTvPictureFacts,
    liveTvReasonText,
    errorView,
    programmeAt,
    gridLayout,
    gridSlots,
    guideEnds,
    filterChannels,
    adjacentChannel,
    dvrAiringKey,
    dvrIndex,
    dvrMarks,
    dvrCaptureProgress,
    dvrAiringActions,
    dvrTunerLine,
    dvrCountdown,
    dvrReminderCountdown,
    dvrRecordingDetail,
  });
});
