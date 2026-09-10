"use strict";

(function publish(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.PlurxLiveTv = api;
})(typeof self !== "undefined" ? self : globalThis, function buildLiveTv() {
  const RETRYABLE_CODES = new Set([
    "owner_unavailable",
    "live_tv_protocol_unready",
    "tuner_capacity",
    "tuner_unavailable",
    "startup_timeout",
    "stream_failed",
    "capability_expired",
  ]);

  function errorView(error) {
    const code = String((error && error.code) || "owner_unavailable");
    const views = {
      live_tv_disabled: ["Live TV is off", "An administrator can enable it in Settings → Developer."],
      live_tv_protocol_unready: ["Live TV is updating", "Every active cluster node must run the compatible Live TV protocol."],
      owner_unavailable: ["Tuner owner unavailable", "The selected tuner owner cannot serve Live TV right now."],
      start_outcome_unknown: ["Start response was lost", "Wait 90 seconds for any unclaimed tuner session to expire, then select the channel again."],
      live_tv_storage_unavailable: ["Live TV needs browser storage", "Allow this site's local storage so an interrupted start cannot acquire a second tuner after reload."],
      tuner_capacity: ["All Live TV slots are busy", "Close another Live TV session or try this channel again shortly."],
      tuner_unavailable: ["The tuner could not start this channel", "A tuner, signal, or channel authorization may be unavailable."],
      channel_not_found: ["Channel no longer available", "Reload the channel list and choose another channel."],
      drm_unsupported: ["Protected channel", "plurx does not play DRM-protected television."],
      codec_unsupported: ["Channel format unsupported", "The tuner owner cannot decode this channel into the compatible live format."],
      startup_timeout: ["Channel took too long to start", "No first live segment arrived before the startup deadline."],
      stream_failed: ["Live stream stopped", "The tuner or transcoder stopped producing live television."],
      capability_expired: ["Live session expired", "The player was idle or disconnected. Start the channel again."],
      settings_conflict: ["Live TV settings changed", "Reload the channel list before starting another channel."],
      guide_unavailable: ["No programme guide yet", "The tuner owner has not fetched a guide. Channels still play; rows show number and callsign only."],
    };
    const selected = views[code] || views.owner_unavailable;
    return { code, title: selected[0], detail: selected[1], retryable: RETRYABLE_CODES.has(code) };
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

  // Source facts from the tuner and the bounded FFmpeg input description.
  // Missing fields stay missing; codecs never imply resolution or layout.
  function channelBadges(channel, nowSeconds, programmeEnd) {
    const badges = sourceDetails(channel, nowSeconds, programmeEnd).compact;
    return badges.filter((badge, index) => badges.indexOf(badge) === index);
  }

  function capability(info) {
    return info && typeof info.session_id === "string" && info.session_id ? info.session_id : null;
  }

  // Only random marker identities are persisted, never capabilities, account
  // tokens, channel IDs or tuner URLs. Each tab owns its own storage key, so
  // clearing a confirmed request cannot erase another tab's uncertain result.
  class StartBarrier {
    constructor(storage, now, randomId) {
      this.storage = storage;
      this.now = now;
      this.randomId = randomId;
      this.prefix = "plurx_live_tv_pending_v1:";
      this.observed = new Map();
    }

    sync() {
      try {
        const storage = this.storage(), found = new Set(), now = this.now();
        for (let i = 0; i < storage.length; i++) {
          const key = storage.key(i);
          if (!key || !key.startsWith(this.prefix)) continue;
          found.add(key);
          const value = storage.getItem(key), old = this.observed.get(key);
          // A reload or another tab's rearm always earns a fresh monotonic
          // deadline. Wall-clock movement never shortens the safety wait.
          if (!old || old.value !== value) this.observed.set(key, { value, until: now + 90000 });
        }
        for (const [key, state] of this.observed) {
          if (!found.has(key)) this.observed.delete(key);
          else if (now >= state.until && storage.getItem(key) === state.value) {
            storage.removeItem(key);
            this.observed.delete(key);
            found.delete(key);
          }
        }
        // Sweep first, then refuse. Refusing before the expiry pass meant a
        // store holding 32 markers could never clear them and Live TV stayed
        // permanently unavailable in that browser.
        if (found.size >= 32) throw new Error("too many unresolved starts");
      } catch (_) { throw { code: "live_tv_storage_unavailable" }; }
    }

    begin() {
      this.sync();
      if (this.observed.size) throw { code: "start_outcome_unknown" };
      return this.hold();
    }

    hold(key) {
      try {
        const storage = this.storage();
        const replacement = this.prefix + this.randomId();
        // Immutable identities: persist the replacement BEFORE retiring the
        // old key. Another tab expiring the old key cannot delete our rearm.
        if (storage.getItem(replacement) !== null) throw new Error("marker collision");
        storage.setItem(replacement, "1");
        if (storage.getItem(replacement) !== "1") throw new Error("storage did not retain marker");
        this.observed.set(replacement, { value: "1", until: this.now() + 90000 });
        this.confirm(key);
        return replacement;
      } catch (_) { throw { code: "live_tv_storage_unavailable" }; }
    }

    confirm(key) {
      if (!key) return;
      try {
        const state = this.observed.get(key), storage = this.storage();
        if (state && storage.getItem(key) === state.value) storage.removeItem(key);
        this.observed.delete(key);
      } catch (_) { /* Keeping a marker is conservative; begin still fails closed. */ }
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

  return Object.freeze({
    Lease,
    StartBarrier,
    channelView,
    channelBadges,
    measuredSourceFormat,
    sourceDetails,
    errorView,
    programmeAt,
    gridLayout,
    gridSlots,
    guideEnds,
    filterChannels,
    adjacentChannel,
  });
});
