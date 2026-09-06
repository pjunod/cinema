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

  return Object.freeze({ Lease, StartBarrier, channelView, errorView });
});
