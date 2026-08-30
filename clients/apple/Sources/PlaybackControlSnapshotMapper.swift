import Foundation

/// Turning what AVPlayer is doing into what the protocol says.
///
/// The rules live here rather than in `PlayerController` for one reason: the
/// web client already decided them (`crates/plurxd/src/web/index.html`,
/// `playbackControlSnapshot`), and three platforms answering the same question
/// differently is worse than any of the three answers. Keeping the mapping as
/// a pure function over an explicit observation means the whole of it is
/// testable, and the part of `PlayerController` that cannot be tested shrinks
/// to filling in fields.
enum PlaybackControlMapping {
    /// A stream whose bytes end within this much of the title's duration has
    /// actually finished. Ending earlier is a truncated stream the legacy
    /// early-end handler reopens, so it stays active failed demand.
    static let endedSlackMs = 15_000

    /// A wait this long is no longer buffering. The web client nudges the
    /// element before this point; past it, the server should be told the
    /// player is starved rather than merely waiting.
    static let persistentStallMs = 8_000

    static let minimumActiveRate = 0.25
    static let maximumRate = 4.0
}

/// What the player is doing, gathered at one instant.
///
/// Every field is something `PlayerController` already knows; nothing here is
/// derived. `AVPlayer` and `AVPlayerItem` do not appear, so a test can state a
/// player state directly instead of building one.
struct PlayerControlObservation: Equatable {
    /// Film position, already rebased onto the title's timeline.
    var positionMs: Int
    /// The title's duration where it is known, `0` where it is not (a growing
    /// stream). Position is clamped to it, as the web client clamps.
    var durationMs: Int
    /// The contiguous buffered range *containing the playhead*, in title time.
    /// Nil when no loaded range contains it — the protocol's runway is what
    /// can be played without a fetch, so a disjoint range ahead is not runway.
    var bufferedFromMs: Int?
    var bufferedThroughMs: Int?
    var rate: Double
    var isPaused: Bool
    /// The bytes ran out. Not necessarily the title: see `endedSlackMs`.
    var isEnded: Bool
    var isSeeking: Bool
    /// True once real playback has begun. Before it, the player is starting
    /// however busy it looks.
    var hasStarted: Bool
    /// How long the player has been waiting for data, or nil if it is not
    /// waiting. `persistentStallMs` separates waiting from stalled.
    var waitingForMs: Int?
    /// The item is playable right now — AVPlayer's likely-to-keep-up. A player
    /// that is not paused and not likely to keep up is waiting even when it
    /// has not reported a wait.
    var isLikelyToKeepUp: Bool
    var errorCode: ClientErrorCode?
    var errorDetail: String?
    var droppedFrames: Int?
    var observedDownloadBps: Int64?
    /// A recovery path knows things AVPlayer cannot report — whether a wait
    /// ran out of bytes or stalled with bytes in hand, and which class of
    /// failure hls saw. It overrides the derived observation, exactly as the
    /// web client lets its recovery callbacks override.
    var observationOverride: ClientObservation?
    /// A render state the controller knows better than this mapping does, for
    /// the same reason.
    var renderOverride: RenderState?
    var selection: ClientSelection
    var capabilities: DynamicCapabilities

    /// The runway ahead of the playhead, in milliseconds. Zero when nothing
    /// contiguous is loaded.
    var runwayMs: Int {
        guard let bufferedThroughMs else { return 0 }
        return max(0, bufferedThroughMs - positionMs)
    }
}

extension PlaybackControlMapping {
    /// The whole mapping. Order matters and follows the web client's: a hard
    /// error outranks an end, an end outranks a controller override, and an
    /// override outranks everything the player itself reports.
    static func snapshot(from observation: PlayerControlObservation) -> PlaybackControlSnapshot {
        let position = clampedPosition(observation)
        let ended = terminallyEnded(observation, position: position)
        let render = renderState(observation, terminallyEnded: ended)
        let demand = demand(observation, terminallyEnded: ended)
        let range = bufferedRange(observation, position: position)
        return PlaybackControlSnapshot(
            demand: demand,
            positionMs: position,
            bufferedFromMs: range.from,
            bufferedThroughMs: range.through,
            playbackRate: playbackRate(observation, demand: demand),
            renderState: render,
            // Only a seek reports a target, and the target *is* the position:
            // the playhead the viewer asked for, not the one AVPlayer is
            // still rendering.
            seekTargetMs: render == .seeking ? position : nil,
            observedDownloadBps: observation.observedDownloadBps
                .flatMap { $0 > 0 ? $0 : nil },
            selection: observation.selection,
            capabilities: observation.capabilities,
            observation: clientObservation(observation, render: render)
        )
    }

    static func clampedPosition(_ observation: PlayerControlObservation) -> Int {
        let position = max(0, observation.positionMs)
        guard observation.durationMs > 0 else { return position }
        return min(position, observation.durationMs)
    }

    /// The bytes ending is not the title ending. A stream that stops well
    /// short of the duration is truncated, and the client reopens it — so it
    /// reports failed rather than ended, and keeps demanding.
    static func terminallyEnded(
        _ observation: PlayerControlObservation,
        position: Int
    ) -> Bool {
        guard observation.isEnded else { return false }
        guard observation.durationMs > 0 else { return true }
        return position >= max(0, observation.durationMs - endedSlackMs)
    }

    static func demand(
        _ observation: PlayerControlObservation,
        terminallyEnded: Bool
    ) -> PlaybackDemand {
        if terminallyEnded { return .end }
        // A non-terminal end is a truncation being recovered from, so the
        // client still wants bytes.
        if observation.isEnded { return .active }
        return observation.isPaused ? .hold : .active
    }

    static func renderState(
        _ observation: PlayerControlObservation,
        terminallyEnded: Bool
    ) -> RenderState {
        if observation.errorCode != nil { return .failed }
        if observation.isEnded { return terminallyEnded ? .ended : .failed }
        if let renderOverride = observation.renderOverride { return renderOverride }
        if observation.isSeeking { return .seeking }
        if !observation.hasStarted { return .starting }
        if let waitingForMs = observation.waitingForMs {
            return waitingForMs >= persistentStallMs ? .stalled : .waiting
        }
        if !observation.isPaused && !observation.isLikelyToKeepUp { return .waiting }
        return .rendering
    }

    /// A player that means to play reports at least a quarter rate even while
    /// its rate is momentarily zero — the protocol reads rate as intent, and
    /// zero from an active player would read as a hold nobody asked for. A
    /// held player reports what it actually has.
    static func playbackRate(
        _ observation: PlayerControlObservation,
        demand: PlaybackDemand
    ) -> Double {
        let rate = observation.rate.isFinite ? observation.rate : 0
        if demand == .active {
            return min(maximumRate, max(minimumActiveRate, rate == 0 ? 1 : rate))
        }
        return min(maximumRate, max(0, rate))
    }

    /// Runway is what can be played without another fetch, so only the range
    /// containing the playhead counts. `through` never precedes the position
    /// even when the loaded range ends just behind it.
    static func bufferedRange(
        _ observation: PlayerControlObservation,
        position: Int
    ) -> (from: Int?, through: Int) {
        guard let through = observation.bufferedThroughMs else {
            return (nil, position)
        }
        let from = observation.bufferedFromMs.map { max(0, min($0, position)) }
        return (from, max(position, through))
    }

    static func clientObservation(
        _ observation: PlayerControlObservation,
        render: RenderState
    ) -> ClientObservation? {
        var value = ClientObservation()
        value.decoderState = observation.errorCode != nil
            ? .failed
            : render == .stalled ? .starved : observation.hasStarted ? .ready : .unknown
        if let droppedFrames = observation.droppedFrames, droppedFrames >= 0 {
            value.droppedFrames = droppedFrames
        }
        value.errorCode = observation.errorCode
        value.errorDetail = observation.errorCode == nil ? nil : observation.errorDetail
        // A recovery path's evidence is more specific than anything derived
        // from the player's own state, so it wins field by field.
        if let override = observation.observationOverride {
            if let decoderState = override.decoderState { value.decoderState = decoderState }
            if let droppedFrames = override.droppedFrames { value.droppedFrames = droppedFrames }
            if let errorCode = override.errorCode {
                value.errorCode = errorCode
                value.errorDetail = override.errorDetail
            }
        }
        return value.bounded
    }
}
