(function (root, factory) {
  const control = factory();
  if (typeof module === "object" && module.exports) module.exports = control;
  root.PlurxPlaybackControl = control;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const PROTOCOL = "plurx-playback-control-v1";
  const MIN_EXCHANGE_MS = 250;
  const MAX_EXCHANGE_MS = 60_000;
  const EXCHANGE_DEADLINE_MS = 6_000;
  const PREPARATION_TTL_MS = 330_000;
  const MAX_MEDIA_MILLIS = 366 * 24 * 60 * 60 * 1_000;
  const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

  // The actions this client will accept from the server, and the only ones the
  // server will send it. Naming an action here is a promise that receiving it
  // is not a protocol error; it is not yet a promise to act on it. Recovery
  // authority still belongs to this client's own timers until the milestone
  // that moves it.
  const SUPPORTED_ACTIONS = Object.freeze([
    "hold", "retry_resource", "terminal", "prepare_replacement",
  ]);

  function defaultNow() {
    if (typeof performance === "object" && typeof performance.now === "function") {
      return performance.now();
    }
    return Date.now();
  }

  function retryDelay(error, fallback) {
    const value = Number(error && error.retryAfterMs);
    return Number.isSafeInteger(value) && value >= 0 && value <= MAX_EXCHANGE_MS
      ? Math.max(MIN_EXCHANGE_MS, value)
      : fallback;
  }

  function validBootstrap(value) {
    return !!value
      && value.protocol === PROTOCOL
      && typeof value.url === "string"
      && /^\/api\/v1\/hls\/[^/]+\/control$/.test(value.url)
      && typeof value.generation === "string"
      && UUID_RE.test(value.generation)
      && Number.isSafeInteger(value.control_epoch)
      && value.control_epoch > 0
      && Number.isSafeInteger(value.next_exchange_ms)
      && value.next_exchange_ms >= MIN_EXCHANGE_MS
      && value.next_exchange_ms <= MAX_EXCHANGE_MS
      && Number.isSafeInteger(value.lease_timeout_ms)
      && value.lease_timeout_ms >= value.next_exchange_ms
      && value.lease_timeout_ms <= 600_000;
  }

  function validSnapshot(value) {
    return !!value
      && ["active", "hold", "end"].includes(value.demand)
      && ["starting", "rendering", "waiting", "stalled", "seeking", "ended", "failed"]
        .includes(value.render_state)
      && Number.isSafeInteger(value.position_ms)
      && value.position_ms >= 0
      && Number.isSafeInteger(value.buffered_through_ms)
      && value.buffered_through_ms >= value.position_ms
      && Number.isFinite(value.playback_rate)
      && value.playback_rate >= 0
      && !!value.selection
      && !!value.capabilities
      && validAcknowledgement(value.acknowledgement)
      && !(value.demand === "end" && value.acknowledgement
        && value.acknowledgement.state === "committed");
  }

  function validAcknowledgement(value) {
    if (value == null) return true;
    if (!value || typeof value !== "object" || !UUID_RE.test(value.action_id)
        || !["metadata_ready", "buffer_ready", "committed", "failed", "aborted"]
          .includes(value.state)) return false;
    const buffered = value.buffered_through_ms;
    const origin = value.committed_media_origin_ms;
    const firstFrame = value.first_frame_unix_ms;
    if (buffered != null && (!Number.isSafeInteger(buffered)
        || buffered < 0 || buffered > MAX_MEDIA_MILLIS)) return false;
    if (origin != null && (!Number.isSafeInteger(origin)
        || origin < 0 || origin > MAX_MEDIA_MILLIS)) return false;
    if (firstFrame != null && (!Number.isSafeInteger(firstFrame) || firstFrame <= 0)) return false;
    if (value.state === "buffer_ready" && buffered == null) return false;
    if (value.state === "committed" && (origin == null || firstFrame == null)) return false;
    return true;
  }

  function boundedObservation(value) {
    if (!value || typeof value !== "object") return null;
    const observation = {};
    if (Number.isSafeInteger(value.dropped_frames) && value.dropped_frames >= 0) {
      observation.dropped_frames = value.dropped_frames;
    }
    if (["unknown", "ready", "starved", "failed"].includes(value.decoder_state)) {
      observation.decoder_state = value.decoder_state;
    }
    if (["network", "manifest", "media", "decoder", "drm", "unknown"]
      .includes(value.error_code)) {
      observation.error_code = value.error_code;
    }
    if (observation.error_code && value.error_detail != null) {
      const detail = String(value.error_detail).replace(/[\r\n\0]/g, " ").slice(0, 120);
      if (detail) observation.error_detail = detail;
    }
    return Object.keys(observation).length ? observation : null;
  }

  function makeRequest(bootstrap, clientInstanceId, sequence, snapshot) {
    if (!validSnapshot(snapshot)) throw new TypeError("invalid playback-control snapshot");
    const request = Object.assign({}, snapshot, {
      protocol: PROTOCOL,
      generation: bootstrap.generation,
      control_epoch: bootstrap.control_epoch,
      client_instance_id: clientInstanceId,
      sequence,
      supported_actions: SUPPORTED_ACTIONS.slice(),
    });
    return request;
  }

  // Capture on the player's synchronous source turn, before any queue/await.
  // The JSON-shaped payload and local owner are recursively copied and frozen;
  // later selection/capability mutation cannot relabel an exact retry.
  function immutableCopy(value) {
    if (!value || typeof value !== "object") return value;
    const copy = Array.isArray(value) ? value.map(immutableCopy)
      : Object.fromEntries(Object.entries(value).map(([key, item]) => [key, immutableCopy(item)]));
    return Object.freeze(copy);
  }

  function capture(snapshot, intentGeneration, owner) {
    if (!validSnapshot(snapshot) || !Number.isSafeInteger(intentGeneration)
        || intentGeneration < 0 || !owner || typeof owner.lifecycleId !== "string"
        || !Number.isSafeInteger(owner.attachmentGeneration)) return null;
    return immutableCopy({ snapshot, intentGeneration, owner });
  }

  function sameIntent(left, right) {
    return !!left && !!right && left.intentGeneration === right.intentGeneration
      && left.owner.lifecycleId === right.owner.lifecycleId
      && left.owner.attachmentGeneration === right.owner.attachmentGeneration;
  }

  function validResponse(bootstrap, request, response) {
    return !!response
      && response.protocol === PROTOCOL
      && response.generation === bootstrap.generation
      && response.control_epoch === bootstrap.control_epoch
      && Number.isSafeInteger(response.accepted_sequence)
      && response.accepted_sequence === request.sequence
      && validAction(response.action);
  }

  // An action outside the declared vocabulary is a protocol error, and the
  // reporter stops: the server was told what this client accepts, so anything
  // else means the two disagree about the contract and continuing would be
  // guessing. A `hold` whose reason is unrecognised is still a hold — the
  // reason is diagnostic, and refusing the exchange over one unknown word
  // would silence this client against a merely newer server.
  function validAction(action) {
    if (!action) return false;
    if (action.type === "none") return true;
    if (action.type === "hold") return typeof action.reason === "string";
    if (action.type === "terminal") {
      return typeof action.code === "string" && typeof action.message === "string";
    }
    if (action.type === "retry_resource") {
      return typeof action.reason === "string"
        && Number.isSafeInteger(action.after_ms)
        && action.after_ms > 0
        && action.after_ms <= MAX_EXCHANGE_MS;
    }
    if (action.type === "prepare") return validPrepareAction(action);
    return false;
  }

  function validPrepareAction(action) {
    if (!action || typeof action !== "object" || !UUID_RE.test(action.action_id)
        || !UUID_RE.test(action.session_id) || typeof action.playlist_url !== "string"
        || action.playlist_url.length > 512 || !Number.isSafeInteger(action.media_origin_ms)
        || action.media_origin_ms < 0 || action.media_origin_ms > MAX_MEDIA_MILLIS) return false;
    const path = action.playlist_url.split(/[?#]/, 1)[0];
    const root = `/api/v1/hls/${action.session_id}/`;
    if (path !== root + "index.m3u8" && path !== root + "master.m3u8") return false;
    const selection = action.effective_selection;
    if (!selection || typeof selection !== "object"
        || Object.keys(selection).length !== 7
        || typeof selection.quality_auto !== "boolean"
        || !Number.isSafeInteger(selection.height) || selection.height < 0 || selection.height > 2160
        || (selection.audio_track !== null && (!Number.isSafeInteger(selection.audio_track)
          || selection.audio_track < 0 || selection.audio_track > 1024))
        || (selection.subtitle_burn !== null && (!Number.isSafeInteger(selection.subtitle_burn)
          || selection.subtitle_burn < 0 || selection.subtitle_burn > 1024))
        || !Number.isSafeInteger(selection.audio_offset_ms)
        || selection.audio_offset_ms < -15_000 || selection.audio_offset_ms > 15_000
        || !["source", "server_selected"].includes(selection.codec)
        || (selection.dynamic_range !== null
          && !["dolby_vision", "hdr10", "hlg", "sdr"].includes(selection.dynamic_range))) {
      return false;
    }
    return true;
  }

  function sameJson(left, right) {
    try { return JSON.stringify(left) === JSON.stringify(right); } catch (_) { return false; }
  }

  // The protocol state for a prepared successor. This deliberately owns no
  // media element: the server does not produce the staged playlist yet. The
  // later two-player work can attach one resource and drive the readiness
  // methods without changing the exchange or teardown rules implemented here.
  class Preparation {
    constructor(options) {
      const value = options || {};
      this.now = typeof value.now === "function" ? value.now : defaultNow;
      this.active = null;
      this.ignoredActionId = null;
      this.lastOutcome = null;
    }

    adopt(action, request) {
      if (!validPrepareAction(action)) return false;
      const receivedAt = this.now();
      this.active = {
        offer: immutableCopy(action),
        requestedSelection: immutableCopy(request && request.selection),
        receivedAt,
        expiresAt: receivedAt + PREPARATION_TTL_MS,
        progress: null,
        acknowledgement: null,
        resource: null,
        release: null,
      };
      this.ignoredActionId = null;
      this.lastOutcome = "offered";
      return true;
    }

    attachResource(actionId, resource, release) {
      if (!this.active || this.active.offer.action_id !== actionId
          || this.active.resource !== null || typeof release !== "function") return false;
      this.active.resource = resource;
      this.active.release = release;
      return true;
    }

    releaseResource(reason) {
      const current = this.active;
      if (!current || current.resource === null) return;
      const resource = current.resource;
      const release = current.release;
      current.resource = null;
      current.release = null;
      try { release(resource, reason, current.offer); } catch (_) {}
    }

    drop(reason, ignore) {
      if (!this.active) return false;
      if (ignore) this.ignoredActionId = this.active.offer.action_id;
      this.releaseResource(reason);
      this.active = null;
      this.lastOutcome = reason;
      return true;
    }

    expired() {
      return !!this.active && this.now() >= this.active.expiresAt;
    }

    acknowledgement(demand) {
      if (!this.active) return null;
      if (this.expired()) {
        this.drop("expired", true);
        return null;
      }
      const acknowledgement = this.active.acknowledgement;
      if (demand === "end" && acknowledgement && acknowledgement.state === "committed") {
        return null;
      }
      return acknowledgement;
    }

    advance(state, fields, currentSelection) {
      if (!this.active || this.expired()) {
        if (this.active) this.drop("expired", true);
        return false;
      }
      const current = this.active;
      const value = fields || {};
      if (state === "metadata_ready") {
        if (current.progress !== null && current.progress !== "metadata_ready") return false;
      } else if (state === "buffer_ready") {
        if (current.progress !== "metadata_ready" && current.progress !== "buffer_ready") return false;
        if (!Number.isSafeInteger(value.bufferedThroughMs)
            || value.bufferedThroughMs < 0 || value.bufferedThroughMs > MAX_MEDIA_MILLIS) return false;
      } else if (state === "committed") {
        if (current.progress !== "buffer_ready") return false;
        if (!Number.isSafeInteger(value.firstFrameUnixMs) || value.firstFrameUnixMs <= 0) return false;
        if (!sameJson(current.requestedSelection, currentSelection)) {
          this.releaseResource("selection_changed");
          current.progress = "aborted";
          current.acknowledgement = immutableCopy({
            action_id: current.offer.action_id,
            state: "aborted",
            buffered_through_ms: null,
            committed_media_origin_ms: null,
            first_frame_unix_ms: null,
          });
          this.lastOutcome = "selection_changed";
          return true;
        }
      } else if (state !== "failed" && state !== "aborted") {
        return false;
      }
      const acknowledgement = {
        action_id: current.offer.action_id,
        state,
        buffered_through_ms: state === "buffer_ready" ? value.bufferedThroughMs : null,
        committed_media_origin_ms: state === "committed" ? current.offer.media_origin_ms : null,
        first_frame_unix_ms: state === "committed" ? value.firstFrameUnixMs : null,
      };
      if (!validAcknowledgement(acknowledgement)) return false;
      current.acknowledgement = immutableCopy(acknowledgement);
      if (state === "failed" || state === "aborted") this.releaseResource(state);
      return true;
    }

    settle(request, response, error) {
      const acknowledgement = request && request.acknowledgement;
      if (error) {
        const retryable = (error.status === 425 && error.code === "owner_transition")
          || (error.status === 429 && error.code === "control_rate_limited")
          || (error.status === 503 && error.code === "control_unavailable")
          || error.status === 408 || error.status === 0 || !Number.isFinite(error.status);
        if (retryable) return "retrying";
        const rejectedCommit = error.status === 409 && error.code === "stale_control"
          && acknowledgement && acknowledgement.state === "committed"
          && this.active && acknowledgement.action_id === this.active.offer.action_id
          && error.generation === request.generation
          && error.controlEpoch === request.control_epoch;
        if (rejectedCommit) {
          this.drop("commit_rejected", false);
          return "commit_rejected";
        }
        this.drop(String(error.code || "exchange_failed"), false);
        return "failed";
      }

      const action = response && response.action;
      if (action && action.type === "prepare") {
        if (this.ignoredActionId === action.action_id) return "ignored";
        if (this.expired()) {
          this.drop("expired", true);
          return "expired";
        }
        if (!this.active) return this.adopt(action, request) ? "offered" : "invalid";
        if (this.active.offer.action_id !== action.action_id) {
          this.drop("replaced", false);
          return this.adopt(action, request) ? "replaced" : "invalid";
        }
        if (!sameJson(this.active.offer, action)) {
          this.drop("invalid_repeat", true);
          return "invalid";
        }
        if (acknowledgement && acknowledgement.action_id === action.action_id) {
          if (acknowledgement.state === "committed") {
            this.releaseResource("commit_discarded");
            this.active.progress = "aborted";
            this.active.acknowledgement = immutableCopy({
              action_id: action.action_id,
              state: "aborted",
              buffered_through_ms: null,
              committed_media_origin_ms: null,
              first_frame_unix_ms: null,
            });
            this.lastOutcome = "commit_discarded";
            return "commit_discarded";
          }
          if (["metadata_ready", "buffer_ready"].includes(acknowledgement.state)) {
            this.active.progress = acknowledgement.state;
            this.active.acknowledgement = null;
          }
        }
        return "repeated";
      }

      this.ignoredActionId = null;
      if (!this.active) return "none";
      if (acknowledgement && acknowledgement.action_id === this.active.offer.action_id) {
        if (acknowledgement.state === "committed") {
          this.drop("committed", false);
          return "committed";
        }
        if (acknowledgement.state === "failed" || acknowledgement.state === "aborted") {
          const outcome = acknowledgement.state;
          this.drop(outcome, false);
          return outcome;
        }
      }
      this.drop("withdrawn", false);
      return "withdrawn";
    }

    stop(reason) {
      this.drop(reason || "stopped", false);
      this.ignoredActionId = null;
    }

    status() {
      const current = this.active;
      return {
        active: !!current,
        offer: current ? current.offer : null,
        progress: current ? current.progress : null,
        acknowledgement: current ? current.acknowledgement : null,
        expires_at_ms: current ? current.expiresAt : null,
        ignored_action_id: this.ignoredActionId,
        last_outcome: this.lastOutcome,
      };
    }
  }

  class Reporter {
    constructor(options) {
      const value = options || {};
      if (!validBootstrap(value.bootstrap)) throw new TypeError("invalid playback-control bootstrap");
      if (typeof value.clientInstanceId !== "string" || !UUID_RE.test(value.clientInstanceId)) {
        throw new TypeError("invalid playback-control client identity");
      }
      if (typeof value.capture !== "function" || typeof value.send !== "function") {
        throw new TypeError("playback-control reporter requires capture and send functions");
      }
      this.bootstrap = Object.assign({}, value.bootstrap);
      this.clientInstanceId = value.clientInstanceId;
      this.capture = value.capture;
      this.send = value.send;
      // Wrapped, not assigned. `setTimeout` and `clearTimeout` are
      // WindowTimers methods and every browser brand-checks their receiver:
      // stored on an object and invoked as `this.setTimer(...)`, the receiver
      // is the reporter and the call throws `TypeError: Illegal invocation`.
      // Node does not brand-check, which is why this survived a test suite
      // that injects its own timer in every case and never exercised the
      // default at all — and why the web control plane completed exactly zero
      // exchanges on real hardware while every unit test passed.
      const scheduleDefault = (run, ms) => globalThis.setTimeout(run, ms);
      const cancelDefault = (handle) => globalThis.clearTimeout(handle);
      this.setTimer = value.setTimer || scheduleDefault;
      this.clearTimer = value.clearTimer || cancelDefault;
      this.now = value.now || defaultNow;
      this.onExchange = typeof value.onExchange === "function" ? value.onExchange : function () {};
      this.sequence = 0;
      this.acceptedSequence = 0;
      this.inFlight = false;
      this.pending = null;
      this.timer = null;
      this.exchangeTimer = null;
      this.abortController = null;
      this.deadlineExceeded = false;
      this.stopped = false;
      this.lastAccepted = null;
      this.lastStartedAt = null;
      this.retryRequest = null;
      this.retryCapture = null;
      this.acceptedCapabilitiesKey = null;
      this.nextAllowedAt = 0;
    }

    start() {
      if (!this.stopped && this.sequence === 0 && !this.inFlight) this.notify();
      return this;
    }

    notify(value) {
      if (this.stopped) return null;
      const newest = value || this.capture();
      if (!newest || !validSnapshot(newest.snapshot)) return null;
      this.pending = newest;
      if (this.timer !== null) {
        this.clearTimer(this.timer);
        this.timer = null;
      }
      this.drain();
      return this.contextFor(newest.snapshot);
    }

    schedule() {
      if (this.stopped || this.inFlight || this.pending || this.timer !== null) return;
      const retrying = this.retryRequest !== null;
      // `nextAllowedAt` is honoured on the ordinary path too, so a server that
      // named its own retry interval is waited out in one timer rather than
      // scheduled at the default and then re-deferred inside `drain`. Both
      // reach the same instant; one is observable.
      const delay = retrying
        ? Math.max(0, this.nextAllowedAt - this.now())
        : Math.max(this.bootstrap.next_exchange_ms, this.nextAllowedAt - this.now());
      this.timer = this.setTimer(() => {
        this.timer = null;
        this.notify();
      }, delay);
    }

    async drain() {
      if (this.stopped || this.inFlight || !this.pending) return;
      const now = this.now();
      const rateAllowedAt = this.lastStartedAt === null
        ? 0 : this.lastStartedAt + MIN_EXCHANGE_MS;
      const allowedAt = Math.max(rateAllowedAt, this.nextAllowedAt);
      if (now < allowedAt) {
        if (this.timer === null) {
          this.timer = this.setTimer(() => {
            this.timer = null;
            this.drain();
          }, allowedAt - now);
        }
        return;
      }
      let request = this.retryRequest;
      let requestCapture = this.retryCapture;
      if (!request) {
        requestCapture = this.pending;
        const snapshot = requestCapture.snapshot;
        this.pending = null;
        const sequence = this.sequence + 1;
        if (!Number.isSafeInteger(sequence)) {
          this.stop();
          return;
        }
        this.sequence = sequence;
        request = makeRequest(this.bootstrap, this.clientInstanceId, sequence, snapshot);
        const capabilityKey = JSON.stringify(snapshot.capabilities);
        if (sequence !== 1 && capabilityKey === this.acceptedCapabilitiesKey) {
          delete request.capabilities;
        }
      }
      this.nextAllowedAt = 0;
      this.lastStartedAt = now;
      this.inFlight = true;
      this.abortController = typeof AbortController === "function" ? new AbortController() : null;
      this.deadlineExceeded = false;
      if (this.abortController) {
        this.exchangeTimer = this.setTimer(() => {
          this.deadlineExceeded = true;
          this.abortController.abort();
        }, EXCHANGE_DEADLINE_MS);
      }
      try {
        const response = await this.send(
          this.bootstrap.url,
          request,
          this.abortController ? this.abortController.signal : undefined,
        );
        if (this.stopped) return;
        if (!validResponse(this.bootstrap, request, response)) {
          const error = new Error("invalid playback-control response");
          error.name = "PlaybackControlProtocolError";
          throw error;
        }
        this.retryRequest = null;
        this.retryCapture = null;
        if (request.capabilities) {
          this.acceptedCapabilitiesKey = JSON.stringify(request.capabilities);
        }
        this.acceptedSequence = Math.max(this.acceptedSequence, response.accepted_sequence);
        this.lastAccepted = {
          generation: request.generation,
          control_epoch: request.control_epoch,
          sequence: response.accepted_sequence,
          demand: request.demand,
          render_state: request.render_state,
          position_ms: request.position_ms,
          buffered_through_ms: request.buffered_through_ms,
          observed_download_bps: request.observed_download_bps == null
            ? null : request.observed_download_bps,
          observation: boundedObservation(request.observation),
        };
        // `retry_resource` paces the next exchange from the server's own
        // cadence rather than this client's guess. It is not a failure, so it
        // does not touch the retry path — the exchange succeeded, and the
        // server simply said when to ask again.
        if (response.action.type === "retry_resource") {
          this.nextAllowedAt = this.now()
            + Math.max(MIN_EXCHANGE_MS, response.action.after_ms);
        }
        this.onExchange({ request, response, error: null, capture:requestCapture,
          intentGeneration:requestCapture.intentGeneration });
        // A terminal verdict ends reporting. It does not tear down the player:
        // this reporter still owns no recovery, and the buffer already fetched
        // is still worth playing. The milestone that moves that authority is
        // the one that acts on this.
        if (request.demand === "end"
            || (response.action.type === "terminal"
              && sameIntent(requestCapture, this.capture()))) {
          this.stop();
          return;
        }
      } catch (error) {
        const canceled = error && error.name === "AbortError" && !this.deadlineExceeded;
        if (!this.stopped && !canceled) {
          let reportedError = error;
          if (error && error.name === "AbortError" && this.deadlineExceeded) {
            reportedError = new Error("playback-control exchange deadline exceeded");
            reportedError.name = "TimeoutError";
          }
          this.onExchange({ request, response: null, error: reportedError,
            capture:requestCapture, intentGeneration:requestCapture.intentGeneration });
          const status = Number(reportedError && reportedError.status);
          const terminalProtocolError = reportedError
            && reportedError.name === "PlaybackControlProtocolError";
          const ownerChanged = status === 409 && reportedError.code === "owner_changed"
            && this.resetForOwner(reportedError);
          const staleGeneration = status === 409 && reportedError.code === "stale_control"
            && reportedError.generation !== request.generation
            && this.resetForOwner(reportedError);
          // A rejected commit has already torn the staged successor down. It
          // is the one stale-control answer this reporter can recover from:
          // the request shape proves which transaction was rejected, while
          // the immutable sequence/client fences make their own failures bugs.
          const preparationRejected = status === 409 && reportedError.code === "stale_control"
            && request.acknowledgement && request.acknowledgement.state === "committed"
            && reportedError.generation === request.generation
            && reportedError.controlEpoch === request.control_epoch;
          const retryableControl = (status === 425 && reportedError.code === "owner_transition")
            || (status === 429 && reportedError.code === "control_rate_limited")
            || (status === 503 && reportedError.code === "control_unavailable");
          const retryableTransport = status === 408 || status === 0 || !Number.isFinite(status);
          if (ownerChanged || staleGeneration) {
            this.nextAllowedAt = this.now() + retryDelay(reportedError, MIN_EXCHANGE_MS);
          } else if (preparationRejected) {
            this.retryRequest = null;
            this.retryCapture = null;
          } else if (!terminalProtocolError && (retryableControl || retryableTransport)) {
            this.retryRequest = request;
            this.retryCapture = requestCapture;
            const fallback = retryableControl ? 500 : this.bootstrap.next_exchange_ms;
            this.nextAllowedAt = this.now() + retryDelay(reportedError, fallback);
          } else {
            this.stop();
          }
        }
      } finally {
        if (this.exchangeTimer !== null) this.clearTimer(this.exchangeTimer);
        this.exchangeTimer = null;
        this.deadlineExceeded = false;
        this.abortController = null;
        this.inFlight = false;
        if (this.stopped) return;
        if (this.pending) this.drain();
        else this.schedule();
      }
    }

    legacyContext() {
      return this.lastAccepted ? Object.assign({}, this.lastAccepted) : null;
    }

    contextFor(snapshot) {
      if (!validSnapshot(snapshot)) return null;
      return {
        generation: this.bootstrap.generation,
        control_epoch: this.bootstrap.control_epoch,
        sequence: null,
        demand: snapshot.demand,
        render_state: snapshot.render_state,
        position_ms: snapshot.position_ms,
        buffered_through_ms: snapshot.buffered_through_ms,
        observed_download_bps: snapshot.observed_download_bps == null
          ? null : snapshot.observed_download_bps,
        observation: boundedObservation(snapshot.observation),
      };
    }

    resetForOwner(error) {
      const generation = error && error.generation;
      const epoch = Number(error && error.controlEpoch);
      const generationChanged = typeof generation === "string"
        && UUID_RE.test(generation) && generation !== this.bootstrap.generation;
      const epochChanged = Number.isSafeInteger(epoch) && epoch > this.bootstrap.control_epoch;
      if (!generationChanged && !epochChanged) return false;
      if (typeof generation !== "string" || !UUID_RE.test(generation)
          || !Number.isSafeInteger(epoch) || epoch <= 0) return false;
      let newest = null;
      try { newest = this.capture(); } catch (_) {}
      if (!newest || !validSnapshot(newest.snapshot)) return false;
      this.bootstrap.generation = generation;
      this.bootstrap.control_epoch = epoch;
      this.sequence = 0;
      this.acceptedSequence = 0;
      this.retryRequest = null;
      this.retryCapture = null;
      this.acceptedCapabilitiesKey = null;
      this.lastAccepted = null;
      this.lastStartedAt = null;
      this.pending = newest;
      return true;
    }

    status() {
      return {
        sequence: this.sequence,
        accepted_sequence: this.acceptedSequence,
        in_flight: this.inFlight,
        pending: this.pending !== null,
        retrying: this.retryRequest !== null,
        stopped: this.stopped,
        next_exchange_ms: this.bootstrap.next_exchange_ms,
        lease_timeout_ms: this.bootstrap.lease_timeout_ms,
      };
    }

    stop() {
      if (this.stopped) return;
      this.stopped = true;
      this.pending = null;
      this.retryRequest = null;
      this.retryCapture = null;
      this.nextAllowedAt = 0;
      if (this.timer !== null) this.clearTimer(this.timer);
      this.timer = null;
      if (this.exchangeTimer !== null) this.clearTimer(this.exchangeTimer);
      this.exchangeTimer = null;
      if (this.abortController) this.abortController.abort();
      this.abortController = null;
    }
  }

  return Object.freeze({
    PROTOCOL, Preparation, Reporter, capture, sameIntent, validBootstrap, validResponse,
    validPrepareAction,
  });
});
