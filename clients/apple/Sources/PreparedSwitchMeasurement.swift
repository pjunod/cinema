import Foundation

/// Measuring a prepared quality switch, with no AVFoundation in it.
///
/// Every function here is pure and every input is a plain number the caller has
/// already read off an access log. That is deliberate in two directions. It puts
/// the arithmetic — the window edges, the two-sided sum, the counter reset —
/// inside reach of the unit lane, which is where this programme's defects have
/// actually lived. And it makes it structurally impossible for the instrument to
/// be the thing it measures: nothing below touches a player, an item, an output
/// or a clock, so recording a switch cannot change the timing of one.
///
/// Nothing here decides anything either. The rows are advisory in the strict
/// sense Paul's standing rule means: no caller reads them back, no capability
/// consults them, and a window that could not be measured is reported as
/// unmeasured rather than as a failure. The bar these readings are compared
/// against lives in `docs/playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md`.
enum PreparedSwitchMeasurement {
    /// Two seconds *either side* of the commit, so a four-second window.
    ///
    /// A prepared switch spans two `AVPlayerItem`s, and each keeps its own
    /// access log: the predecessor's counters stop at `replaceCurrentItem` and
    /// the successor's start there. Reading one item's delta across the swap
    /// would measure half the window and report it as the whole.
    static let windowMs = 2_000

    /// How many readings each side keeps. A bound, so a long film cannot grow it.
    static let samplesMax = 32

    /// One reading of one cumulative counter.
    struct Sample: Equatable {
        let atMs: Int
        let count: Int

        init(atMs: Int, count: Int) {
            self.atMs = atMs
            self.count = count
        }
    }

    /// What a window came to. `count` is nil when the window could not be
    /// measured at all — never zero, because a zero nobody observed and a zero
    /// that was observed are different facts and only one of them clears a bar.
    struct Window: Equatable {
        let count: Int?
        let beforeMs: Int
        let afterMs: Int
        let windowMs: Int

        var coveredMs: Int { beforeMs + afterMs }
        var spanMs: Int { windowMs * 2 }
    }

    /// The delta of one cumulative counter over one side of the window.
    ///
    /// Nil with fewer than two readings inside it: a delta needs two, and
    /// inventing the missing one is how an instrument comes to certify a window
    /// it never saw.
    private static func side(
        _ samples: [Sample],
        from: Int,
        to: Int
    ) -> (delta: Int, coveredMs: Int)? {
        let kept = samples.filter { $0.atMs >= from && $0.atMs <= to }
            .sorted { $0.atMs < $1.atMs }
        guard let first = kept.first, let last = kept.last, kept.count >= 2 else { return nil }
        // A counter that went backwards was reset — a new item, a renderer
        // rebuild. Everything counted since the reset is inside the window, so
        // the reading is the counter itself and never a negative delta.
        let delta = last.count >= first.count ? last.count - first.count : last.count
        return (max(0, delta), last.atMs - first.atMs)
    }

    /// The two-sided counter delta around a commit.
    ///
    /// `before` is the predecessor item's series and `after` the successor's;
    /// each is clipped to its own side of `commitAtMs` so one reading cannot be
    /// counted twice. Either may be empty, which is a real outcome.
    static func counterDelta(
        before: [Sample],
        after: [Sample],
        commitAtMs: Int,
        windowMs: Int = PreparedSwitchMeasurement.windowMs
    ) -> Window {
        let bound = windowMs > 0 ? windowMs : PreparedSwitchMeasurement.windowMs
        let left = side(before, from: commitAtMs - bound, to: commitAtMs)
        let right = side(after, from: commitAtMs, to: commitAtMs + bound)
        if left == nil, right == nil {
            return Window(count: nil, beforeMs: 0, afterMs: 0, windowMs: bound)
        }
        return Window(
            count: (left?.delta ?? 0) + (right?.delta ?? 0),
            beforeMs: left?.coveredMs ?? 0,
            afterMs: right?.coveredMs ?? 0,
            windowMs: bound
        )
    }

    /// Tap to the successor's first frame, in wall time. Nil rather than a
    /// negative number: two clocks that disagree are not a measurement.
    static func visibleInMs(tappedAtUnixMs: Int?, firstFrameUnixMs: Int?) -> Int? {
        guard let tappedAtUnixMs, let firstFrameUnixMs else { return nil }
        guard firstFrameUnixMs >= tappedAtUnixMs else { return nil }
        return firstFrameUnixMs - tappedAtUnixMs
    }

    private static func seconds(_ ms: Int) -> String {
        String(format: "%.1f s", Double(ms) / 1_000)
    }

    /// One wording, shared with the web and Android ledgers.
    static func framesRow(_ window: Window?) -> String {
        guard let window, let count = window.count else { return "Not measured" }
        return "\(count) dropped · ±\(seconds(window.windowMs)) · "
            + "\(seconds(window.coveredMs)) of \(seconds(window.spanMs)) sampled"
    }

    /// The audio row. Apple's observation is the access log's own stall count,
    /// which is a cumulative counter and so reduces the same way the frames do.
    static func audioRow(_ window: Window?) -> String {
        guard let window, let count = window.count else { return "Not measured" }
        let word = count == 1 ? "stall" : "stalls"
        return "\(count) access-log \(word) · ±\(seconds(window.windowMs)) · "
            + "\(seconds(window.coveredMs)) of \(seconds(window.spanMs)) sampled"
    }

    static func visibleRow(_ ms: Int?) -> String {
        guard let ms else { return "Not measured" }
        return "\(ms) ms"
    }

    /// The three rows the ledger shows.
    struct Reading: Equatable {
        let frames: String
        let audio: String
        let visibleIn: String

        static let unmeasured = Reading(
            frames: "Not measured",
            audio: "Not measured",
            visibleIn: "Not measured"
        )
    }

    /// The recorder the controller pushes into.
    ///
    /// Deliberately dumb: two bounded arrays and four assignments. The reading
    /// is computed on demand, so a closed info panel costs nothing and a commit
    /// costs three stores.
    struct Recorder {
        private var before: [Sample] = []
        private var after: [Sample] = []
        private var beforeStalls: [Sample] = []
        private var afterStalls: [Sample] = []
        private(set) var commitAtMs: Int?
        private var tappedAtUnixMs: Int?
        private var firstFrameUnixMs: Int?

        private static func push(_ into: inout [Sample], _ sample: Sample) {
            // A repeated reading of an unchanged counter at the same instant is
            // noise; a repeated instant would also make `coveredMs` lie.
            if let last = into.last, last.atMs == sample.atMs { into.removeLast() }
            into.append(sample)
            if into.count > samplesMax { into.removeFirst(into.count - samplesMax) }
        }

        /// A reading of the item that is currently on the layer. Which side of
        /// the commit it lands on is decided by whether a commit has happened
        /// yet, because `replaceCurrentItem` is the instant the item changes.
        mutating func note(atMs: Int, droppedFrames: Int?, stalls: Int?) {
            let committed = commitAtMs != nil
            if let droppedFrames {
                if committed { Self.push(&after, Sample(atMs: atMs, count: droppedFrames)) }
                else { Self.push(&before, Sample(atMs: atMs, count: droppedFrames)) }
            }
            if let stalls {
                if committed { Self.push(&afterStalls, Sample(atMs: atMs, count: stalls)) }
                else { Self.push(&beforeStalls, Sample(atMs: atMs, count: stalls)) }
            }
        }

        /// The viewer tapped. Recorded in wall time because the first frame is.
        mutating func note(tappedAtUnixMs: Int) {
            self.tappedAtUnixMs = tappedAtUnixMs
        }

        /// The successor is on the layer. Assignments only.
        mutating func note(commitAtMs: Int) {
            self.commitAtMs = commitAtMs
            firstFrameUnixMs = nil
        }

        /// The successor rendered. Recorded once; a later frame does not move it.
        mutating func note(firstFrameUnixMs: Int) {
            guard commitAtMs != nil, self.firstFrameUnixMs == nil else { return }
            self.firstFrameUnixMs = firstFrameUnixMs
        }

        /// A new staging. The samples taken for the last switch are not this
        /// one's, and the tap is: the wait for an offer happens before this.
        mutating func reopened() {
            before = after
            after = []
            beforeStalls = afterStalls
            afterStalls = []
            commitAtMs = nil
            firstFrameUnixMs = nil
        }

        func reading() -> Reading {
            guard let commitAtMs else { return .unmeasured }
            return Reading(
                frames: framesRow(counterDelta(before: before, after: after, commitAtMs: commitAtMs)),
                audio: audioRow(
                    counterDelta(before: beforeStalls, after: afterStalls, commitAtMs: commitAtMs)
                ),
                visibleIn: visibleRow(
                    visibleInMs(tappedAtUnixMs: tappedAtUnixMs, firstFrameUnixMs: firstFrameUnixMs)
                )
            )
        }
    }

    /// What an audible-seam measurement would need on this platform, and whether
    /// each condition holds.
    ///
    /// Advisory in the strict sense: there is no switch attached to any of
    /// these, nothing reads them back, and a condition that is not met never
    /// refuses a quality change, never disables the prepared handoff, and never
    /// turns a row into a failure. `AVPlayerItemAccessLog`'s stall count is the
    /// one audio observation that costs the audible path nothing, which is why
    /// it is the one that ships.
    struct Requirement: Identifiable, Equatable {
        let title: String
        let detail: String
        let met: Bool

        var id: String { title }
    }

    static func audioRequirements(accessLogAvailable: Bool) -> [Requirement] {
        [
            Requirement(
                title: "Access-log stall count",
                detail: "Met when the item keeps an access log. A read of a log AVFoundation "
                    + "already writes: it adds nothing to the audio path and cannot mute it.",
                met: accessLogAvailable
            ),
            Requirement(
                title: "Waveform gap detection",
                detail: "Not available. An MTAudioProcessingTap is installed by setting the item's "
                    + "audioMix, which puts a real-time callback of ours inside the audible "
                    + "path on every session — a callback that overruns is a silence the "
                    + "viewer hears. The stall count above is what ships instead.",
                met: false
            ),
        ]
    }
}
