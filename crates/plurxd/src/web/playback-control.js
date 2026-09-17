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

  // The only action whose declared name and wire tag differ, and the one
  // mistake in this protocol that fails silently in both directions.
  // `prepare_replacement` is the transaction (roadmap §3.1) and is what
  // `supported_actions` must carry: `accepts()` on the server is a literal
  // string comparison, so a client that declares `prepare` is simply never
  // offered a successor. `prepare` is the moment, and is what arrives as
  // `action.type`: a client that switches on `prepare_replacement` never
  // fires. Neither failure produces an error anywhere.
  const PREPARE_REPLACEMENT_ACTION = "prepare_replacement";
  const PREPARE_ACTION_TAG = "prepare";
  // Bounds the server already enforces, restated here so a malformed
  // preparation is refused before it reaches a second decode pipeline rather
  // than after. Sources: crates/plurxd/src/transcode.rs MAX_HEIGHT, and
  // playback_control.rs MAX_MEDIA_MILLIS / EffectiveSelection / is_node_relative_playlist.
  const MAX_HEIGHT = 2160;
  const MAX_MEDIA_MS = 366 * 24 * 60 * 60 * 1_000;
  const MAX_AUDIO_OFFSET_MS = 15_000;
  const MAX_TRACK_INDEX = 1_024;
  const MAX_PLAYLIST_URL_BYTES = 512;
  // Not codec names. `source` and `server_selected` distinguish
  // direct-play/remux from transcode — the DeliveryMethod axis — and a client
  // that renders either to a viewer as a codec is wrong.
  const DELIVERY_CODECS = Object.freeze(["source", "server_selected"]);
  const DYNAMIC_RANGES = Object.freeze(["dolby_vision", "hdr10", "hlg", "sdr"]);
  // Exactly five. A sixth is a serde error on the server, which answers 400
  // with no `invalid_field` at all, so the client gets no help identifying it.
  const ACKNOWLEDGEMENT_STATES = Object.freeze([
    "metadata_ready", "buffer_ready", "committed", "failed", "aborted",
  ]);

  // The actions this client will accept from the server, and the only ones the
  // server will send it. Naming an action here is a promise that receiving it
  // is not a protocol error; it is not yet a promise to act on it. Recovery
  // authority still belongs to this client's own timers until the milestone
  // that moves it.
  const SUPPORTED_ACTIONS = Object.freeze([
    "hold", "retry_resource", "terminal", PREPARE_REPLACEMENT_ACTION,
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

  function boundedInteger(value, min, max) {
    return Number.isSafeInteger(value) && value >= min && value <= max;
  }

  // A preparation's playlist is node-relative and belongs to the successor
  // session named in the same action: exactly
  // `/api/v1/hls/{session_id}/index.m3u8` or `.../master.m3u8`, with a query
  // or fragment allowed and ignored. Anything else is refused here, without a
  // request — the relay path already refuses to point a client at another
  // origin, and a client that resolved an absolute URL would undo that.
  function preparedPlaylistUrl(sessionId, url) {
    if (typeof sessionId !== "string" || !sessionId || sessionId.includes("/")) return null;
    if (typeof url !== "string" || !url || url.length > MAX_PLAYLIST_URL_BYTES) return null;
    const cut = url.search(/[?#]/);
    const path = cut === -1 ? url : url.slice(0, cut);
    const base = `/api/v1/hls/${sessionId}/`;
    return path === `${base}index.m3u8` || path === `${base}master.m3u8` ? url : null;
  }

  // What the successor will deliver — the server's answer, not what was asked
  // for. Unknown keys are tolerated: `ControlAction` carries no
  // `deny_unknown_fields`, so a later server may add one and this client must
  // not refuse the preparation over a field it does not read.
  function validEffectiveSelection(value) {
    return !!value
      && typeof value === "object"
      && typeof value.quality_auto === "boolean"
      && boundedInteger(value.height, 0, MAX_HEIGHT)
      && (value.audio_track == null || boundedInteger(value.audio_track, 0, MAX_TRACK_INDEX))
      && (value.subtitle_burn == null || boundedInteger(value.subtitle_burn, 0, MAX_TRACK_INDEX))
      && boundedInteger(value.audio_offset_ms, -MAX_AUDIO_OFFSET_MS, MAX_AUDIO_OFFSET_MS)
      && DELIVERY_CODECS.includes(value.codec)
      && (value.dynamic_range == null || DYNAMIC_RANGES.includes(value.dynamic_range));
  }

  // A malformed preparation is fatal, exactly like any other malformed action.
  // Softening it into an ignore would leave this client half-understanding a
  // staging the server is holding a real encoder open for.
  // The server accepts anything `uuid::Uuid::parse_str` takes for an
  // `action_id` — any version, any variant, and the braced, simple and URN
  // forms — while today it only ever mints a v4. Matching its shape rather
  // than the strict RFC one is deliberate: refusing a legitimate staging is a
  // protocol error that stops this reporter for the rest of the session, and a
  // relayed action minted by a peer on a later version would do exactly that.
  const ACTION_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  function validPreparation(action) {
    return !!action
      && action.type === PREPARE_ACTION_TAG
      && typeof action.action_id === "string" && ACTION_ID_RE.test(action.action_id)
      && typeof action.session_id === "string" && action.session_id !== ""
      && preparedPlaylistUrl(action.session_id, action.playlist_url) !== null
      && boundedInteger(action.media_origin_ms, 0, MAX_MEDIA_MS)
      && validEffectiveSelection(action.effective_selection);
  }

  // The four rules a careless client hits as 400s, and one that is a design
  // rule rather than a field check: a `committed` may not share an exchange
  // with `demand: "end"`. Switching and then closing is two exchanges — the
  // commit, then the end — in that order.
  //
  // `committed_media_origin_ms` is the one a client will get wrong by omission
  // rather than by error, because it is easy to read the state machine and
  // conclude the commit only owes a timestamp. It owes both, and the reason is
  // that `action_id` says *which offer* is being answered while the origin says
  // *what was built*: a successor prepared for one point in the film and
  // committed after the viewer seeked elsewhere is otherwise indistinguishable
  // from a correct commit, and the server would publish it on the strength of
  // this acknowledgement alone. It must equal the `media_origin_ms` of the
  // offer verbatim — the server drops a commit that names a different one,
  // silently — so echo the field, never a recomputed value.
  function validAcknowledgement(value, demand) {
    return !!value
      && typeof value === "object"
      && typeof value.action_id === "string" && ACTION_ID_RE.test(value.action_id)
      && ACKNOWLEDGEMENT_STATES.includes(value.state)
      && (value.buffered_through_ms == null
        || boundedInteger(value.buffered_through_ms, 0, MAX_MEDIA_MS))
      && (value.first_frame_unix_ms == null
        || (Number.isSafeInteger(value.first_frame_unix_ms) && value.first_frame_unix_ms > 0))
      && (value.committed_media_origin_ms == null
        || boundedInteger(value.committed_media_origin_ms, 0, MAX_MEDIA_MS))
      && (value.state !== "buffer_ready" || value.buffered_through_ms != null)
      && (value.state !== "committed" || value.first_frame_unix_ms != null)
      && (value.state !== "committed" || value.committed_media_origin_ms != null)
      && (value.state !== "committed" || demand !== "end");
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
      && (value.acknowledgement == null
        || validAcknowledgement(value.acknowledgement, value.demand));
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
    if (action.type === PREPARE_ACTION_TAG) return validPreparation(action);
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
          const retryableControl = (status === 425 && reportedError.code === "owner_transition")
            || (status === 429 && reportedError.code === "control_rate_limited")
            || (status === 503 && reportedError.code === "control_unavailable");
          const retryableTransport = status === 408 || status === 0 || !Number.isFinite(status);
          if (ownerChanged) {
            this.nextAllowedAt = this.now() + retryDelay(reportedError, MIN_EXCHANGE_MS);
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

  // ---- M3: measuring the prepared switch -----------------------------------
  //
  // Pure arithmetic, deliberately kept out of the player. Nothing below reads
  // a DOM node, starts a timer, or touches the commit path: the player hands
  // these functions samples it has already taken and they answer a question.
  // That is the whole reason they can be unit-tested at all, and the reason
  // instrumenting the switch cannot change it.
  //
  // The window is two seconds EITHER SIDE of the commit, so four seconds wide.
  // A prepared switch spans two counters — the predecessor's and the
  // successor's — because each media element keeps its own cumulative
  // `droppedVideoFrames`. Adding the two halves is the only honest reading;
  // taking a single element's delta across the swap would measure one half and
  // call it the whole.
  const PREPARED_SWITCH_WINDOW_MS = 2_000;
  // The seam window is centred on the swap and is much narrower: a gap in the
  // sound at the moment the element changes is what a viewer hears, and a
  // silence three seconds later is a supply problem, not a seam.
  const PREPARED_SWITCH_SEAM_WINDOW_MS = 300;
  const PREPARED_SWITCH_SILENCE_GAP_MS = 20;
  // Linear amplitude, not dBFS. -50 dBFS: below any dither or room floor a
  // decoder emits, above the exact zeros a silent-but-present stream carries.
  const PREPARED_SWITCH_SILENCE_FLOOR = 0.003;

  function finiteNumber(value) {
    return typeof value === "number" && Number.isFinite(value);
  }

  /// Samples of ONE cumulative counter, clipped to its side of the commit and
  /// to the window. Returns null when fewer than two samples survive, because
  /// a delta needs two readings and inventing one is how an instrument comes
  /// to report zero for a window it never observed.
  function preparedSwitchSeries(samples, from, to) {
    if (!Array.isArray(samples)) return null;
    const kept = samples.filter((sample) =>
      sample && finiteNumber(sample.at) && finiteNumber(sample.count)
      && sample.at >= from && sample.at <= to);
    if (kept.length < 2) return null;
    kept.sort((left, right) => left.at - right.at);
    const first = kept[0];
    const last = kept[kept.length - 1];
    // A counter that went backwards was reset — a new element, a new item,
    // a driver reload. Everything it has counted since is inside the window,
    // so the reading is the counter itself rather than a negative delta.
    const delta = last.count >= first.count ? last.count - first.count : last.count;
    return { delta: Math.max(0, Math.round(delta)), coveredMs: last.at - first.at };
  }

  /// The two-sided counter delta around a commit.
  ///
  /// `before` is the predecessor's series and `after` the successor's. Either
  /// may be absent — a commit whose successor never reported twice is a real
  /// outcome — and the result says how much of the four seconds was actually
  /// covered so a zero can be told apart from a zero nobody watched.
  function preparedSwitchCounterDelta(options) {
    const commitAtMs = options && options.commitAtMs;
    if (!finiteNumber(commitAtMs)) return null;
    const windowMs = finiteNumber(options.windowMs) && options.windowMs > 0
      ? options.windowMs : PREPARED_SWITCH_WINDOW_MS;
    const before = preparedSwitchSeries(options.before, commitAtMs - windowMs, commitAtMs);
    const after = preparedSwitchSeries(options.after, commitAtMs, commitAtMs + windowMs);
    if (!before && !after) {
      return { count: null, coveredMs: 0, windowMs, spanMs: windowMs * 2,
        beforeMs: 0, afterMs: 0 };
    }
    return {
      count: (before ? before.delta : 0) + (after ? after.delta : 0),
      beforeMs: before ? before.coveredMs : 0,
      afterMs: after ? after.coveredMs : 0,
      coveredMs: (before ? before.coveredMs : 0) + (after ? after.coveredMs : 0),
      windowMs,
      spanMs: windowMs * 2,
    };
  }

  /// The longest run of consecutive samples at or under the silence floor,
  /// in milliseconds.
  ///
  /// Time-domain samples, not a frequency bin: a 20 ms gap is 960 samples at
  /// 48 kHz and no animation frame can see it, which is why the scan is over
  /// the buffer rather than over the callbacks. `n` samples span `n /
  /// sampleRate` seconds — the run's own length, not the gap between its ends,
  /// so a single silent sample is one sample period and not zero.
  function preparedSwitchSilenceRunMs(samples, sampleRate, floor) {
    if (!samples || typeof samples.length !== "number" || samples.length === 0) return null;
    if (!finiteNumber(sampleRate) || sampleRate <= 0) return null;
    const threshold = finiteNumber(floor) ? Math.abs(floor) : PREPARED_SWITCH_SILENCE_FLOOR;
    let longest = 0;
    let run = 0;
    for (let index = 0; index < samples.length; index += 1) {
      const value = samples[index];
      if (finiteNumber(value) && Math.abs(value) <= threshold) {
        run += 1;
        if (run > longest) longest = run;
      } else {
        run = 0;
      }
    }
    return (longest / sampleRate) * 1_000;
  }

  /// Whether the sound had a seam at the swap.
  ///
  /// `scans` are per-buffer results: `at` is the monotonic clock at the END of
  /// the buffer, `spanMs` the buffer's duration, `runMs` the longest silent run
  /// inside it. Buffers that are silent end to end chain together, because a
  /// 60 ms gap arrives as two whole 42 ms buffers and reporting the longest
  /// single buffer would call it 42 ms and a seam threshold of 20 ms would
  /// still be met — but a gap spanning three buffers of 15 ms each would not.
  function preparedSwitchSeam(scans, swapAtMs, options) {
    if (!Array.isArray(scans) || !finiteNumber(swapAtMs)) return null;
    const windowMs = options && finiteNumber(options.windowMs) && options.windowMs > 0
      ? options.windowMs : PREPARED_SWITCH_SEAM_WINDOW_MS;
    const gapMs = options && finiteNumber(options.gapMs) && options.gapMs > 0
      ? options.gapMs : PREPARED_SWITCH_SILENCE_GAP_MS;
    const half = windowMs / 2;
    const kept = scans.filter((scan) =>
      scan && finiteNumber(scan.at) && finiteNumber(scan.runMs) && finiteNumber(scan.spanMs)
      && scan.spanMs > 0 && scan.at >= swapAtMs - half && scan.at <= swapAtMs + half);
    if (!kept.length) return null;
    kept.sort((left, right) => left.at - right.at);
    let longest = 0;
    let chain = 0;
    let coveredMs = 0;
    for (const scan of kept) {
      coveredMs += scan.spanMs;
      const whollySilent = scan.runMs >= scan.spanMs;
      chain = whollySilent ? chain + scan.spanMs : 0;
      const candidate = Math.max(chain, scan.runMs);
      if (candidate > longest) longest = candidate;
    }
    return {
      gapMs: longest,
      seam: longest >= gapMs,
      thresholdMs: gapMs,
      coveredMs,
      windowMs,
      scans: kept.length,
    };
  }

  /// Tap to the successor's first frame. Null rather than a negative number:
  /// two clocks that disagree are not a measurement.
  function preparedSwitchVisibleInMs(tappedAtMs, firstFrameAtMs) {
    if (!finiteNumber(tappedAtMs) || !finiteNumber(firstFrameAtMs)) return null;
    if (firstFrameAtMs < tappedAtMs) return null;
    return Math.round(firstFrameAtMs - tappedAtMs);
  }

  function seconds(ms) {
    return `${(ms / 1_000).toFixed(1)} s`;
  }

  /// One wording for the ledger row on every platform. The count comes first
  /// because it is the measurement; the coverage follows because a zero over
  /// half the window is a different fact from a zero over all of it.
  function preparedSwitchFramesRow(delta) {
    if (!delta || delta.count == null) return "Not measured";
    return `${delta.count} dropped · ±${seconds(delta.windowMs)} · `
      + `${seconds(delta.coveredMs)} of ${seconds(delta.spanMs)} sampled`;
  }

  /// The audio row for a platform that counts events (Apple stalls, Android
  /// audio-sink underruns) rather than scanning the waveform.
  function preparedSwitchAudioEventRow(delta, noun) {
    if (!delta || delta.count == null) return "Not measured";
    const word = delta.count === 1 ? noun : `${noun}s`;
    return `${delta.count} ${word} · ±${seconds(delta.windowMs)} · `
      + `${seconds(delta.coveredMs)} of ${seconds(delta.spanMs)} sampled`;
  }

  /// The audio row for a platform that scans the waveform.
  function preparedSwitchAudioSeamRow(seam) {
    if (!seam) return "Not measured";
    return seam.seam
      ? `gap ${Math.round(seam.gapMs)} ms in ${seconds(seam.windowMs)} of the swap`
      : `no gap ≥ ${Math.round(seam.thresholdMs)} ms in ${seconds(seam.windowMs)} of the swap`;
  }

  function preparedSwitchVisibleRow(ms) {
    return ms == null ? "Not measured" : `${ms} ms`;
  }

  return Object.freeze({ PROTOCOL, PREPARE_REPLACEMENT_ACTION, PREPARE_ACTION_TAG,
    SUPPORTED_ACTIONS, Reporter, capture, sameIntent, validBootstrap, validResponse,
    validPreparation, validAcknowledgement, preparedPlaylistUrl,
    PREPARED_SWITCH_WINDOW_MS, PREPARED_SWITCH_SEAM_WINDOW_MS,
    PREPARED_SWITCH_SILENCE_GAP_MS, PREPARED_SWITCH_SILENCE_FLOOR,
    preparedSwitchCounterDelta, preparedSwitchSilenceRunMs, preparedSwitchSeam,
    preparedSwitchVisibleInMs, preparedSwitchFramesRow,
    preparedSwitchAudioEventRow, preparedSwitchAudioSeamRow,
    preparedSwitchVisibleRow });
});
