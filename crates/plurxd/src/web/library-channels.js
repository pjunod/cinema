"use strict";

// Pure Library-channel client policy. DOM rendering stays in the app shell;
// ordering, stale-response fencing, clock estimation, and resumable editor
// state live here so native/web fixtures can exercise the same contract.
(function publish(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.PlurxLibraryChannels = api;
})(typeof self !== "undefined" ? self : globalThis, function buildLibraryChannels() {
  const MODES = new Set(["following", "paused", "personal"]);
  const STEPS = ["content", "playback", "channel"];

  function defaultRecipe() {
    return {
      version: 1, library_ids: [], kinds: ["movie", "episode"],
      genres_any: [], tags_any: [], keywords_any: [], year_min: null,
      year_max: null, include_item_ids: [], include_show_ids: [],
      exclude_item_ids: [], exclude_show_ids: [], ordering: "balanced_shuffle",
      include_specials: false, auto_refresh: true, match_all_in_scope: false,
    };
  }

  function emptyDraft(owner) {
    return {
      server: String(owner && owner.server || ""),
      user: String(owner && owner.user || ""), step: "content", dirty: false,
      request_id: null, expected_revision: null, id: null, name: "",
      description: "", visibility: "personal", enabled: false,
      recipe: defaultRecipe(), preview: null,
    };
  }

  function cleanStrings(raw) {
    const seen = new Map();
    for (const part of String(raw || "").split(",")) {
      const value = part.trim();
      if (value && !seen.has(value.toLocaleLowerCase())) seen.set(value.toLocaleLowerCase(), value);
    }
    return Array.from(seen.values());
  }

  function validateDraft(draft) {
    const fields = {};
    const name = String(draft && draft.name || "").trim();
    if (!name || Array.from(name).length > 80) fields.name = "Use 1–80 characters.";
    if (Array.from(String(draft && draft.description || "")).length > 500)
      fields.description = "Use at most 500 characters.";
    const recipe = draft && draft.recipe || {};
    if (!Array.isArray(recipe.kinds) || !recipe.kinds.length)
      fields.kinds = "Choose movies, episodes, or both.";
    if (draft && draft.enabled && (!draft.preview || !draft.preview.eligible_count))
      fields.enabled = "Preview at least one eligible title before enabling.";
    return fields;
  }

  function nextStep(step, direction) {
    const index = Math.max(0, STEPS.indexOf(step));
    return STEPS[Math.max(0, Math.min(STEPS.length - 1, index + direction))];
  }

  // NTP-style midpoint estimate from a resolve round trip. Device wall time
  // never advances the schedule after this point; performance.now() does.
  class ServerClock {
    constructor(monotonic) {
      this.monotonic = monotonic || (() => performance.now());
      this.baseServer = 0; this.baseMono = 0;
    }
    observe(serverNowMs, sentMono, receivedMono) {
      const end = receivedMono == null ? this.monotonic() : receivedMono;
      this.baseServer = Number(serverNowMs) + Math.max(0, end - Number(sentMono || end)) / 2;
      this.baseMono = end;
    }
    now() { return this.baseServer + Math.max(0, this.monotonic() - this.baseMono); }
  }

  class TuneFence {
    constructor() { this.sequence = 0; this.abort = null; this.mode = null; }
    begin(channelId) {
      if (this.abort) this.abort.abort();
      this.abort = typeof AbortController !== "undefined" ? new AbortController() : null;
      this.mode = "following";
      return {sequence: ++this.sequence, channelId: String(channelId), signal: this.abort && this.abort.signal};
    }
    current(intent) {
      return !!intent && intent.sequence === this.sequence && this.mode === "following"
        && !(intent.signal && intent.signal.aborted);
    }
    setMode(mode) {
      if (!MODES.has(mode)) throw new TypeError("unknown Library-channel playback mode");
      this.mode = mode;
      if (mode !== "following") { ++this.sequence; if (this.abort) this.abort.abort(); }
    }
    stop() { ++this.sequence; this.mode = null; if (this.abort) this.abort.abort(); }
  }

  function byStartThenChannel(left, right) {
    return Number(left.starts_at_ms) - Number(right.starts_at_ms)
      || String(left.channel_id).localeCompare(String(right.channel_id));
  }

  function groupGuide(programmes) {
    const grouped = new Map();
    for (const programme of Array.isArray(programmes) ? programmes : []) {
      const id = String(programme.channel_id || "");
      if (!id) continue;
      if (!grouped.has(id)) grouped.set(id, []);
      grouped.get(id).push(programme);
    }
    for (const rows of grouped.values()) rows.sort(byStartThenChannel);
    return grouped;
  }

  function errorView(error, context) {
    const status = Number(error && error.status || 0);
    const collectionRoute = Boolean(context && context.collection);
    const code = String(error && error.code ||
      (status === 404 && collectionRoute ? "channel_route_unavailable" : "channel_request_failed"));
    const known = {
      channel_occurrence_changed: ["The programme changed", "Resolving the channel again is safe."],
      channel_unavailable: ["Channel unavailable", "It may be disabled or deleted."],
      channel_empty: ["No schedule yet", "Edit the channel and preview an eligible title."],
      scheduled_media_unavailable: ["Programme unavailable", "The pinned edition changed or cannot be used. The shared schedule continues."],
      channel_revision_changed: ["Channel changed elsewhere", "Keep this form and compare it with the latest definition."],
      channel_build_busy: ["Schedule builder busy", "Retry shortly; the previous schedule remains active."],
      channel_store_unavailable: ["Library channels unavailable", "The server cannot establish authoritative channel state."],
      channel_route_unavailable: ["Library channels unavailable", "This server does not expose the Library channels API."],
      channel_request_failed: ["Library channels unavailable", "The request failed before channel state could be read."],
    };
    const selected = known[code] || known.channel_request_failed;
    return {code, title: selected[0], detail: selected[1]};
  }

  return {STEPS, defaultRecipe, emptyDraft, cleanStrings, validateDraft, nextStep,
    ServerClock, TuneFence, groupGuide, errorView};
});
