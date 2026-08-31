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
  const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

  // The actions this client will accept from the server, and the only ones the
  // server will send it. Naming an action here is a promise that receiving it
  // is not a protocol error; it is not yet a promise to act on it. Recovery
  // authority still belongs to this client's own timers until the milestone
  // that moves it.
  const SUPPORTED_ACTIONS = Object.freeze(["hold", "retry_resource", "terminal"]);

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
      && !!value.capabilities;
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
    return false;
  }

  class Reporter {
    constructor(options) {
      const value = options || {};
      if (!validBootstrap(value.bootstrap)) throw new TypeError("invalid playback-control bootstrap");
      if (typeof value.clientInstanceId !== "string" || !UUID_RE.test(value.clientInstanceId)) {
        throw new TypeError("invalid playback-control client identity");
      }
      if (typeof value.snapshot !== "function" || typeof value.send !== "function") {
        throw new TypeError("playback-control reporter requires snapshot and send functions");
      }
      this.bootstrap = Object.assign({}, value.bootstrap);
      this.clientInstanceId = value.clientInstanceId;
      this.snapshot = value.snapshot;
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
      this.acceptedCapabilitiesKey = null;
      this.nextAllowedAt = 0;
    }

    start() {
      if (!this.stopped && this.sequence === 0 && !this.inFlight) this.notify();
      return this;
    }

    notify(snapshot) {
      if (this.stopped) return null;
      const newest = snapshot || this.snapshot();
      if (!validSnapshot(newest)) return null;
      this.pending = newest;
      if (this.timer !== null) {
        this.clearTimer(this.timer);
        this.timer = null;
      }
      this.drain();
      return this.contextFor(newest);
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
      if (!request) {
        const snapshot = this.pending;
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
        this.onExchange({ request, response, error: null });
        // A terminal verdict ends reporting. It does not tear down the player:
        // this reporter still owns no recovery, and the buffer already fetched
        // is still worth playing. The milestone that moves that authority is
        // the one that acts on this.
        if (request.demand === "end" || response.action.type === "terminal") {
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
          this.onExchange({ request, response: null, error: reportedError });
          const status = Number(reportedError && reportedError.status);
          const terminalProtocolError = reportedError
            && reportedError.name === "PlaybackControlProtocolError";
          const ownerChanged = status === 409 && reportedError.code === "owner_changed"
            && this.resetForOwner(reportedError);
          const retryableControl = (status === 425 && reportedError.code === "owner_transition")
            || (status === 429 && reportedError.code === "control_rate_limited")
            || (status === 503 && reportedError.code === "control_unavailable");
          const retryableTransport = status === 408 || status === 0 || !Number.isFinite(status);
          if (ownerChanged) {
            this.nextAllowedAt = this.now() + retryDelay(reportedError, MIN_EXCHANGE_MS);
          } else if (!terminalProtocolError && (retryableControl || retryableTransport)) {
            this.retryRequest = request;
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
      try { newest = this.snapshot(); } catch (_) {}
      if (!validSnapshot(newest)) return false;
      this.bootstrap.generation = generation;
      this.bootstrap.control_epoch = epoch;
      this.sequence = 0;
      this.acceptedSequence = 0;
      this.retryRequest = null;
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
      this.nextAllowedAt = 0;
      if (this.timer !== null) this.clearTimer(this.timer);
      this.timer = null;
      if (this.exchangeTimer !== null) this.clearTimer(this.exchangeTimer);
      this.exchangeTimer = null;
      if (this.abortController) this.abortController.abort();
      this.abortController = null;
    }
  }

  return Object.freeze({ PROTOCOL, Reporter, validBootstrap, validResponse });
});
