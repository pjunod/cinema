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

  function validBootstrap(value) {
    return !!value
      && value.protocol === PROTOCOL
      && typeof value.url === "string"
      && /^\/api\/v1\/hls\/[^/]+\/control$/.test(value.url)
      && typeof value.generation === "string"
      && value.generation.length > 0
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

  function makeRequest(bootstrap, clientInstanceId, sequence, snapshot) {
    if (!validSnapshot(snapshot)) throw new TypeError("invalid playback-control snapshot");
    const request = Object.assign({}, snapshot, {
      protocol: PROTOCOL,
      generation: bootstrap.generation,
      control_epoch: bootstrap.control_epoch,
      client_instance_id: clientInstanceId,
      sequence,
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
      && response.action
      && response.action.type === "none";
  }

  class Reporter {
    constructor(options) {
      const value = options || {};
      if (!validBootstrap(value.bootstrap)) throw new TypeError("invalid playback-control bootstrap");
      if (typeof value.clientInstanceId !== "string" || value.clientInstanceId.length === 0) {
        throw new TypeError("invalid playback-control client identity");
      }
      if (typeof value.snapshot !== "function" || typeof value.send !== "function") {
        throw new TypeError("playback-control reporter requires snapshot and send functions");
      }
      this.bootstrap = Object.assign({}, value.bootstrap);
      this.clientInstanceId = value.clientInstanceId;
      this.snapshot = value.snapshot;
      this.send = value.send;
      this.setTimer = value.setTimer || setTimeout;
      this.clearTimer = value.clearTimer || clearTimeout;
      this.now = value.now || Date.now;
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
    }

    start() {
      if (!this.stopped && this.sequence === 0 && !this.inFlight) this.notify();
      return this;
    }

    notify(snapshot) {
      if (this.stopped) return;
      const newest = snapshot || this.snapshot();
      if (!validSnapshot(newest)) return;
      this.pending = newest;
      if (this.timer !== null) {
        this.clearTimer(this.timer);
        this.timer = null;
      }
      this.drain();
    }

    schedule() {
      if (this.stopped || this.inFlight || this.pending || this.timer !== null) return;
      this.timer = this.setTimer(() => {
        this.timer = null;
        this.notify();
      }, this.bootstrap.next_exchange_ms);
    }

    async drain() {
      if (this.stopped || this.inFlight || !this.pending) return;
      const now = this.now();
      if (this.lastStartedAt !== null) {
        const elapsed = Math.max(0, now - this.lastStartedAt);
        if (elapsed < MIN_EXCHANGE_MS) {
          if (this.timer === null) {
            this.timer = this.setTimer(() => {
              this.timer = null;
              this.drain();
            }, MIN_EXCHANGE_MS - elapsed);
          }
          return;
        }
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
        };
        this.onExchange({ request, response, error: null });
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
          const terminalHttp = status >= 400 && status < 500 && status !== 408 && status !== 429;
          if (terminalProtocolError || terminalHttp) this.stop();
          else this.retryRequest = request;
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
