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
    const protectedChannel = !!(channel && (channel.drm || channel.support === "drm_unsupported"));
    return {
      disabled: protectedChannel,
      label: protectedChannel ? "Protected · unsupported" : "Available",
      tone: protectedChannel ? "warn" : "ready",
    };
  }

  function capability(info) {
    return info && typeof info.session_id === "string" && info.session_id ? info.session_id : null;
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
      if (!id || this.releasing || typeof this.requests.keepalive !== "function") return null;
      const result = await this.requests.keepalive(id);
      return generation === this.generation && this.current === info ? result : null;
    }

    async status() {
      const generation = this.generation;
      const info = this.current;
      const id = capability(info);
      if (!id || typeof this.requests.status !== "function") return null;
      const result = await this.requests.status(id);
      return generation === this.generation && this.current === info ? result : null;
    }
  }

  return Object.freeze({ Lease, channelView, errorView });
});
