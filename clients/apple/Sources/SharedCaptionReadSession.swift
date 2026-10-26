import Combine
import Foundation
import SwiftUI

struct SharedCaptionAsset: Decodable, Identifiable {
    let assetId: String, profileId: String, modelId: String, modelSha256: String, contentRevision: String
    let audioIndex: Int, spokenLanguage: String, outputLanguage: String, task: CaptionTask, durationMs: Int
    var id: String { assetId }
    func validate() throws {
        guard [assetId,profileId,modelSha256,contentRevision].allSatisfy(CaptionValidation.digest),
              (0...255).contains(audioIndex),(1...14_400_000).contains(durationMs),
              !modelId.isEmpty,modelId.utf8.count <= 64,!spokenLanguage.isEmpty,spokenLanguage.utf8.count <= 16,
              outputLanguage == (task == .translate ? "en" : spokenLanguage) else { throw CaptionProtocolError.invalid }
    }
    var profile: CaptionProfile {
        CaptionProfile(profileId: profileId,modelId: modelId,modelSha256: modelSha256,task: task,
            label: nil,default: nil,outputLanguage: outputLanguage,inputLanguages: [spokenLanguage],timelineContract: "source-audio-clock-v1",
            limits: CaptionLimits(segmentBytes: 16384,segmentCues: 128,pageBytes: 65536,pageEntries: 64,
                coverageIntervals: 8,concurrentFetches: 2,cueMemoryBytes: 524288,cueCount: 1024,
                cueWindowBeforeMs: 30000,cueWindowAfterMs: 90000,pollMinMs: 1000,pollMaxMs: 5000,
                retryMaxMs: 30000,heartbeatIntervalMs: 5000))
    }
}
struct SharedCaptionCatalog: Decodable { let version: Int, assets: [SharedCaptionAsset], nextAsset: String? }
struct SharedCaptionManifest: Decodable {
    let version: Int, assetId: String, contentRevision: String, profileId: String, audioIndex: Int
    let spokenLanguage: String, outputLanguage: String, task: CaptionTask, durationMs: Int
    let manifestRevision: String, state: String, reason: String?
    let snapshot: CaptionSnapshot, coverage: [CaptionRange], segments: [CaptionSegmentReference]
}

/// Existing media-read authority permits these observations, never inference
/// demand, missing-range priority, Source model setup, Stop or Retry.
@MainActor
final class SharedCaptionReadSession: ObservableObject {
    @Published private(set) var assets: [SharedCaptionAsset] = []
    @Published private(set) var selected: SharedCaptionAsset?
    @Published private(set) var window: CaptionRenderWindow?
    @Published private(set) var status = "Read captions already published by the Source."
    @Published private(set) var nextAsset: String?
    @Published var offsetMs = 0 { didSet { publish() } }
    private var client: SharedDecisionClient?
    private var context: PlaybackFileContext?
    private var audio: Int?
    private var clock: (() -> Int)?
    private var currentOwner: (() -> Bool)?
    private var generation = 0, windowGeneration = 0, renderRevision = 0
    private var loop: Task<Void,Never>?, refreshTask: Task<Void,Never>?, leaseExpiry: Task<Void,Never>?
    private var polling = false
    private var catalogLoaded = false
    private var cache: [String:(String,[CaptionCue])] = [:]
    private var cues: [CaptionCue] = []
    private var manifestRevision = "0"
    private var requestedPosition: Int?
    private var decoder: JSONDecoder { let value=JSONDecoder();value.keyDecodingStrategy = .convertFromSnakeCase;return value }
    var eligibleAssets: [SharedCaptionAsset] { assets.filter { $0.audioIndex == audio } }

    func configure(client: SharedDecisionClient,context: PlaybackFileContext,audio: Int?,clock: @escaping () -> Int,current: @escaping () -> Bool) {
        if self.context?.sourceKey == context.sourceKey,self.audio == audio {
            self.clock=clock;self.currentOwner=current;publish();return
        }
        retire();self.client=client;self.context=context;self.audio=audio;self.clock=clock;self.currentOwner=current
        let captured=generation
        loop=Task { [weak self] in
            while let self,self.current(captured),self.selected == nil,!self.catalogLoaded {
                await self.catalog(captured)
                if self.catalogLoaded { return }
                do { try await Task.sleep(for:.seconds(3)) } catch { return }
            }
        }
    }
    private func current(_ captured: Int) -> Bool { generation == captured && currentOwner?() == true && context.map { (try? $0.path("captions")) != nil } == true && !Task.isCancelled }
    private func catalog(_ captured: Int,after: String? = nil) async {
        guard let client,let context else { return }
        do {
            let query=after.map { [URLQueryItem(name:"after",value:$0)] } ?? []
            let bytes=try await client.captions(context:context,resource:"captions",query:query)
            guard current(captured) else { return }
            let value=try decoder.decode(SharedCaptionCatalog.self,from:bytes)
            guard value.version==1,value.assets.count<=16,value.nextAsset.map(CaptionValidation.digest) ?? true else { throw CaptionProtocolError.invalid }
            for asset in value.assets { try asset.validate() }
            let ids=value.assets.map(\.assetId)
            guard Set(ids).count==ids.count,ids==ids.sorted(),after.map({ cursor in ids.allSatisfy { $0>cursor } }) ?? true, value.nextAsset.map({ next in (after.map { next>$0 } ?? true) && (ids.last.map { next >= $0 } ?? true) }) ?? true else { throw CaptionProtocolError.invalid }
            assets=value.assets;nextAsset=value.nextAsset;catalogLoaded=true
            status=eligibleAssets.isEmpty ? "No published captions for this audio. Ask the Source administrator to produce them." : "Choose captions already published by the Source."
        } catch { fail(error,captured) }
    }
    func more() { let captured=generation;refreshTask=Task { await catalog(captured,after:nextAsset) } }
    func refresh() {
        let captured=generation
        guard current(captured) else { window=nil;cues=[];cache=[:];return }
        refreshTask=Task { if selected == nil { await catalog(captured) } else { await poll(captured) } }
    }
    func choose(_ asset: SharedCaptionAsset) {
        guard eligibleAssets.contains(where: { $0.assetId==asset.assetId }) else { return }
        deselect();selected=asset;status="Reading available caption coverage…"
        let captured=generation
        loop=Task { [weak self] in
            while let self,self.current(captured),self.selected != nil {
                await self.poll(captured)
                do { try await Task.sleep(for:.seconds(3)) } catch { return }
            }
        }
    }
    func deselect() { leaseExpiry?.cancel();leaseExpiry=nil;windowGeneration+=1;loop?.cancel();loop=nil;refreshTask?.cancel();refreshTask=nil;selected=nil;window=nil;cache=[:];cues=[];manifestRevision="0";requestedPosition=nil }
    func retire() { generation+=1;deselect();client=nil;context=nil;audio=nil;clock=nil;currentOwner=nil;assets=[];nextAsset=nil;polling=false;catalogLoaded=false }
    func mediaChanged(seekTo: Int? = nil) {
        if let seekTo { requestedPosition=seekTo;windowGeneration+=1;window=nil;cues=[];refresh() }
        if let requestedPosition,abs((clock?() ?? requestedPosition)-requestedPosition)<1000 { self.requestedPosition=nil }
        publish()
    }
    private func poll(_ captured: Int) async {
        guard current(captured),!polling,let client,let context,let asset=selected else { return }
        polling=true;defer { if generation==captured { polling=false } }
        let owner=windowGeneration,position=max(0,requestedPosition ?? clock?() ?? 0)
        do {
            let started=ProcessInfo.processInfo.systemUptime
            let bytes=try await client.captions(context:context,resource:"captions/\(asset.assetId)/manifest",query:[URLQueryItem(name:"position_ms",value:String(position))])
            guard current(captured),owner==windowGeneration,selected?.assetId==asset.assetId else { return }
            let manifest=try decoder.decode(SharedCaptionManifest.self,from:bytes)
            guard manifest.version==1,manifest.assetId==asset.assetId,manifest.contentRevision==asset.contentRevision,
                  manifest.profileId==asset.profileId,manifest.audioIndex==asset.audioIndex,
                  manifest.spokenLanguage==asset.spokenLanguage,manifest.outputLanguage==asset.outputLanguage,
                  manifest.task==asset.task,manifest.durationMs==asset.durationMs,
                  CaptionValidation.counter(manifest.manifestRevision),CaptionValidation.compare(manifest.manifestRevision,manifestRevision) != .orderedAscending,
                  manifest.snapshot.revision==manifest.manifestRevision,CaptionValidation.uuid(manifest.snapshot.leaseId),
                  manifest.snapshot.expiresInMs>0,manifest.snapshot.expiresInMs<=30000,
                  manifest.segments.count<=8,manifest.coverage.count<=8,
                  Set(manifest.segments.map(\.segmentId)).count==manifest.segments.count,
                  ["partial","complete","stopped","failed","suspended"].contains(manifest.state) else { throw CaptionProtocolError.invalid }
            let deadline=started+Double(manifest.snapshot.expiresInMs)/1000
            guard ProcessInfo.processInfo.systemUptime<deadline else { throw CaptionProtocolError.expired }
            var kept: [String:(String,[CaptionCue])] = [:],result:[CaptionCue]=[],seen:Set<String>=[]
            var memory=0
            for reference in manifest.segments {
                guard CaptionValidation.digest(reference.segmentId),CaptionValidation.digest(reference.digest),
                      CaptionValidation.range(reference.startMs,reference.endMs,duration:asset.durationMs),
                      reference.endMs>max(0,position-30000),reference.startMs<position+90000 else { throw CaptionProtocolError.invalid }
                let segmentCues:[CaptionCue]
                if let existing=cache[reference.segmentId],existing.0==reference.digest { segmentCues=existing.1 }
                else {
                    let data=try await client.captions(context:context,resource:"captions/\(asset.assetId)/segments/\(reference.segmentId)",query:[URLQueryItem(name:"lease",value:manifest.snapshot.leaseId)],maximumBytes:16384)
                    guard CaptionValidation.sha256(data)==reference.digest else { throw CaptionProtocolError.invalid }
                    let segment=try decoder.decode(CaptionSegment.self,from:data)
                    try segment.validate(reference:reference,asset:asset.assetId,source:asset.contentRevision,profile:asset.profile,duration:asset.durationMs)
                    segmentCues=segment.cues
                }
                guard current(captured),owner==windowGeneration,ProcessInfo.processInfo.systemUptime<deadline else { throw CaptionProtocolError.expired }
                kept[reference.segmentId]=(reference.digest,segmentCues)
                for cue in segmentCues where seen.insert(cue.cueId).inserted { memory+=cue.text.utf8.count;result.append(cue) }
                guard result.count<=1024,memory<=524288 else { throw CaptionProtocolError.budget }
            }
            guard current(captured),owner==windowGeneration,selected?.assetId==asset.assetId else { return }
            cache=kept;cues=result.sorted { $0.startMs<$1.startMs };manifestRevision=manifest.manifestRevision
            let covering=manifest.coverage.contains { $0.startMs<=position && $0.endMs>position }
            for range in manifest.coverage { guard CaptionValidation.range(range.startMs,range.endMs,duration:asset.durationMs) else { throw CaptionProtocolError.invalid } }
            status=covering ? (manifest.state=="complete" ? "Published captions complete." : "Published captions available here.") : "Not captioned here yet. Ask the Source administrator to produce this range."
            publish()
            leaseExpiry?.cancel()
            leaseExpiry=Task { [weak self] in
                do { try await Task.sleep(for:.seconds(max(0,deadline-ProcessInfo.processInfo.systemUptime))) } catch { return }
                guard let self,self.generation==captured,self.windowGeneration==owner else { return }
                self.window=nil;self.cues=[];self.cache=[:]
                self.status="Caption read lease expired. Refreshing Source access…"
            }
        } catch { fail(error,captured) }
    }
    private func publish() {
        guard current(generation),selected != nil else { window=nil;cues=[];cache=[:];return }
        if window?.cues != cues || window?.offsetMs != offsetMs {
            renderRevision+=1;window=CaptionRenderWindow(revision:renderRevision,baseMs:0,offsetMs:offsetMs,cues:cues)
        }
    }
    private func fail(_ error: Error,_ captured: Int) {
        guard generation==captured,!(error is CancellationError) else { return }
        // Cached text cannot substitute for a new grant-authorized read.
        leaseExpiry?.cancel();leaseExpiry=nil;window=nil;cues=[];cache=[:]
        if (error as? APIError)?.httpStatus == 425 {
            status="Checking the Source content before reading published captions. Playback continues."
        } else { status="Shared caption coverage is unavailable. Refresh access; missing ranges need the Source administrator." }
    }
}


struct SharedCaptionReadControls: View {
    @ObservedObject var controller: SharedPlayerController
    @ObservedObject var session: SharedCaptionReadSession
    @State private var requested: SharedCaptionAsset?
    @State private var confirmingLocal=false
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(session.status)
                    Text("Only captions already published by the Source are available here. Missing ranges require the Source administrator; this viewer cannot start or prioritize generation.").font(.caption)
                    Button("Off") { Task { await controller.change(SharedDirectedChange(subtitleIndex:.some(nil))) } }
                    ForEach(session.eligibleAssets) { asset in
                        Button("\(asset.outputLanguage) · \(asset.task == .translate ? "English translation" : "Speech captions")") {
                            if controller.player.isExternalPlaybackActive { requested=asset;confirmingLocal=true }
                            else { Task { await controller.selectCaption(asset) } }
                        }
                    }
                    if session.nextAsset != nil { Button("More published captions") { session.more() } }
                    Button("Refresh available captions") { session.refresh() }
                } header: { Text("Published Source captions") }
                Section {
                    HStack {
                        Button("Earlier 0.25 s") { session.offsetMs=max(-10000,session.offsetMs-250) }
                        Text(String(format:"%+.2f s",Double(session.offsetMs)/1000))
                        Button("Later 0.25 s") { session.offsetMs=min(10000,session.offsetMs+250) }
                    }
                    Text("Captions are drawn on this device. Choose Off or an original caption track before AirPlay.").font(.caption)
                } header: { Text("Local caption timing and output") }
            }
            .navigationTitle("Published captions")
            .toolbar { ToolbarItem(placement:.cancellationAction) { Button("Done") { dismiss() } } }
            .confirmationDialog("Return playback to this device for captions?",isPresented:$confirmingLocal,titleVisibility:.visible) {
                Button("Use captions on this device") { controller.useLocalCaptionOutput() }
                Button("Keep current output",role:.cancel) { requested=nil }
            }
            .onReceive(controller.player.publisher(for: \.isExternalPlaybackActive)) { external in
                if !external,let asset=requested { requested=nil;Task { await controller.selectCaption(asset) } }
            }
        }
    }
}
