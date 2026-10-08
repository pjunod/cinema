import SwiftUI
import CoreImage.CIFilterBuiltins
#if os(iOS)
import VisionKit
#endif

struct RemotePairingApprovalView: View {
    @EnvironmentObject private var remote: RemoteClientModel
    var body: some View {
        if let pending = remote.pairings.first {
            VStack(spacing: 24) {
                Text("Allow \(pending.controllerName) to control Cinema?").font(.title2)
                Text("Only this signed-in account can pair. Approval is local to this TV.")
                HStack {
                    Button("Approve") { remote.approve(pending, allow: true) }
                    Button("Decline") { remote.approve(pending, allow: false) }
                }
                .buttonStyle(.borderedProminent)
            }
            .padding(40)
            .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 20))
            .remoteRestricted()
        } else if let challenge = remote.challenge {
            VStack(spacing: 20) {
                Text("Pair a Cinema phone").font(.title2.bold())
                if let instance = remote.serverInstanceID,
                   let image = RemotePairingQR.image(instance: instance, challenge: challenge) {
                    Image(uiImage: image).interpolation(.none).resizable().scaledToFit().frame(width: 240, height: 240)
                        .accessibilityLabel("Pairing QR code")
                }
                Text(challenge.code).font(.system(size: 44, weight: .bold, design: .monospaced))
                Text("On the phone, open Settings → Remotes & devices, select this TV and scan. Approve the request here.")
                Text("The code expires after two minutes. Pairing grants no administration access.").font(.caption)
                Button("Close") { remote.hidePairing() }.buttonStyle(.bordered)
            }
            .padding(36)
            .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 20))
            .remoteRestricted()
        }
    }
}
enum RemotePairingQR {
    static func payload(instance: String, challenge: CinemaRemoteChallenge) -> String? {
        var url = URLComponents()
        url.scheme = "cinema-remote"; url.host = "pair"
        url.queryItems = [
            .init(name: "server_instance_id", value: instance),
            .init(name: "owner_node_id", value: challenge.target.ownerNodeID),
            .init(name: "session_id", value: challenge.target.sessionID.uuidString),
            .init(name: "receiver_epoch", value: challenge.target.receiverEpoch.uuidString),
            .init(name: "challenge_id", value: challenge.challengeID.uuidString)
        ]
        url.fragment = "code=" + challenge.code
        return url.string
    }
    static func image(instance: String, challenge: CinemaRemoteChallenge) -> UIImage? {
        guard let payload = payload(instance: instance, challenge: challenge) else { return nil }
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        guard let output = filter.outputImage, let image = CIContext().createCGImage(output, from: output.extent) else { return nil }
        return UIImage(cgImage: image)
    }
    static func parse(_ payload: String, instance: String, target: CinemaRemoteTarget) -> (UUID, String)? {
        guard payload.utf8.count <= 2_048, let url = URLComponents(string: payload), url.scheme == "cinema-remote", url.host == "pair", url.user == nil, url.password == nil else { return nil }
        let pairs = url.queryItems ?? []
        guard pairs.count == 5, Set(pairs.map(\.name)).count == 5 else { return nil }
        let values = Dictionary(uniqueKeysWithValues: pairs.map { ($0.name, $0.value ?? "") })
        guard values["server_instance_id"] == instance, values["owner_node_id"] == target.ownerNodeID,
              values["session_id"].flatMap(UUID.init(uuidString:)) == target.sessionID,
              values["receiver_epoch"].flatMap(UUID.init(uuidString:)) == target.receiverEpoch,
              let challenge = values["challenge_id"].flatMap(UUID.init(uuidString:)),
              let fragment = url.fragment, fragment.hasPrefix("code=") else { return nil }
        let code = String(fragment.dropFirst(5))
        guard code.utf8.count == 8, code.utf8.allSatisfy({ (48...57).contains($0) }) else { return nil }
        return (challenge, code)
    }
}

#if os(iOS)
struct RemoteSuggestionCard: View {
    @EnvironmentObject private var remote: RemoteClientModel
    var body: some View {
        if !remote.remotePresented, !remote.suggestionDevices.isEmpty {
            HStack {
                if remote.suggestionDevices.count == 1, let device = remote.suggestionDevices.first {
                    Button("\(device.name) is ready · Open remote") { remote.select(device) }
                    Button("Dismiss", systemImage: "xmark") { remote.dismissSuggestion(device.id) }
                } else {
                    Button("Choose a screen") { remote.remotePresented = true }
                    Button("Dismiss", systemImage: "xmark") { remote.suggestionDevices.forEach { remote.dismissSuggestion($0.id) } }
                }
            }
            .padding()
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
        }
    }
}
struct RemoteCompanionView: View {
    @EnvironmentObject private var remote: RemoteClientModel
    @Environment(\.dismiss) private var dismiss
    @State private var code = ""
    @State private var challenge = ""
    @State private var scanning = false
    @State private var searchText = ""
    @State private var textNonce: UUID?
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 22) {
                    if let device = remote.selectedDevice {
                        Text(device.name).font(.title2.bold())
                        if remote.selectedIsPaired { remoteControls }
                        else { pairingControls(device) }
                        Toggle("Suggest this screen when available", isOn: Binding(get: { remote.suggestionsEnabled(for: device.id) }, set: { remote.setSuggestions($0, for: device.id) }))
                        Button("Choose another screen") { remote.closeController() }
                    } else {
                        Text(remote.status).font(.callout)
                        ForEach(remote.devices) { device in
                            Button { remote.select(device) } label: {
                                HStack { Text(device.name); Spacer(); Text(device.available ? (device.busy ? "In use" : "Available") : "Offline") }
                            }
                            .buttonStyle(.bordered)
                            .disabled(!device.available)
                        }
                        if !remote.unavailableNodes.isEmpty { Text("Some server nodes did not answer discovery.").font(.caption) }
                        if remote.devices.isEmpty { Text("Enable Cinema remotes in Developer settings on the phone and TV, and enable the server's remote feature.") }
                    }
                    Text(remote.commandStatus).font(.caption).accessibilityIdentifier("cinema-remote-outcome")
                }
                .padding(24)
                .frame(maxWidth: 680)
                .frame(maxWidth: .infinity)
            }
            .navigationTitle("Cinema remote")
            .toolbar { Button("Close") { remote.releaseControl(); dismiss() } }
            .onDisappear { remote.stopHolding(); remote.closeController() }
            .onChange(of: remote.selectedDevice?.target) { _, _ in challenge = ""; code = "" }
            .onChange(of: remote.selectedDevice?.id) { _, _ in challenge = ""; code = "" }
            .onChange(of: remote.controllerState?.textNonce) { _, nonce in textNonce = nonce; searchText = "" }
            .sheet(isPresented: $scanning) {
                QRCodeScannerView { payload in
                    scanning = false
                    guard let instance = remote.serverInstanceID, let target = remote.selectedDevice?.target,
                          let parsed = RemotePairingQR.parse(payload, instance: instance, target: target) else { return }
                    challenge = parsed.0.uuidString
                    code = parsed.1
                }
            }
        }
    }
    private func pairingControls(_ device: CinemaRemoteDevice) -> some View {
        VStack(spacing: 14) {
            Text("Choose Pair a phone on the TV. Scan its QR or enter its eight-digit code.")
            if DataScannerViewController.isSupported && DataScannerViewController.isAvailable {
                Button("Scan TV pairing code", systemImage: "qrcode.viewfinder") { scanning = true }
            }
            TextField("Eight-digit TV code", text: $code).keyboardType(.numberPad).textFieldStyle(.roundedBorder)
            Button("Request pairing") {
                guard code.utf8.count == 8, code.allSatisfy({ $0.isASCII && $0.isNumber }) else { return }
                remote.pair(device: device, challengeID: UUID(uuidString: challenge), code: code)
            }
            .buttonStyle(.borderedProminent)
        }
    }
    private func supports(_ action: CinemaRemoteAction.Kind) -> Bool {
        remote.controllerState?.capabilities.contains(action) == true
    }
    private var remoteControls: some View {
        VStack(spacing: 20) {
            if !remote.controlling {
                Text(remote.controllerControl.map { "\($0.controllerName) is controlling this screen." } ?? "This phone is ready to request control.")
                Button(remote.controllerControl == nil ? "Use as remote" : "Take over") { remote.acquire(takeover: remote.controllerControl != nil) }
                    .buttonStyle(.borderedProminent)
            }
            Text(remote.controllerState?.focusedLabel ?? "Waiting for a safe TV focus target")
            RemoteDirectionPad()
            HStack {
                Button("Back") { remote.send(.init(type: .back)) }
                Button("Home") { remote.send(.init(type: .home)) }
                if supports(.stop) { Button("Stop") { remote.send(.init(type: .stop)) } }
            }.buttonStyle(.bordered)
            if let playback = remote.controllerState?.playback {
                Text(playback.title).font(.headline)
                HStack {
                    if supports(.seekRelative) { Button("−10 s") { remote.send(.init(type: .seekRelative, seconds: -10)) } }
                    if supports(.setPlaying) { Button(playback.playing ? "Pause" : "Play") { remote.send(.init(type: .setPlaying, playing: !playback.playing)) } }
                    if supports(.seekRelative) { Button("+10 s") { remote.send(.init(type: .seekRelative, seconds: 10)) } }
                }.buttonStyle(.bordered)
                ForEach(CinemaRemoteTrackKind.allRemoteCases.filter { kind in playback.tracks.contains { $0.kind == kind } }, id: \.rawValue) { kind in
                    if supports(.openTracks) { Button("Choose " + kind.rawValue) { remote.send(.init(type: .openTracks, kind: kind)) } }
                    if remote.controllerState?.route == "tracks" && supports(.chooseTrack) {
                        ForEach(playback.tracks.filter { $0.kind == kind }, id: \.optionID) { option in
                            Button(option.label) { remote.send(.init(type: .chooseTrack, kind: kind, optionID: option.optionID)) }
                        }
                    }
                }
            }
            if let nonce = remote.controllerState?.textNonce {
                TextField("Text for TV search", text: $searchText).textFieldStyle(.roundedBorder)
                Button("Send search text") {
                    guard textNonce == nonce else { return }
                    remote.send(.init(type: .textReplace, textNonce: nonce, text: searchText))
                }
            }
        }
    }
}
private struct RemoteDirectionPad: View {
    @EnvironmentObject private var remote: RemoteClientModel
    @State private var holding: CinemaRemoteDirection?
    var body: some View {
        VStack(spacing: 12) {
            direction(.up, label: "Up", symbol: "chevron.up")
            HStack(spacing: 18) {
                direction(.left, label: "Left", symbol: "chevron.left")
                Button("Select") { remote.send(.init(type: .select)) }.frame(width: 92, height: 72).buttonStyle(.borderedProminent)
                direction(.right, label: "Right", symbol: "chevron.right")
            }
            direction(.down, label: "Down", symbol: "chevron.down")
        }
        .disabled(!remote.controlling)
    }
    private func direction(_ direction: CinemaRemoteDirection, label: String, symbol: String) -> some View {
        Image(systemName: symbol).font(.title2).frame(width: 80, height: 64)
            .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 12))
            .contentShape(Rectangle()).accessibilityLabel(label).accessibilityAddTraits(.isButton)
            .accessibilityAction { remote.send(.init(type: .navigate, direction: direction)) }
            .gesture(DragGesture(minimumDistance: 0).onChanged { _ in
                if holding != direction { holding = direction; remote.beginHolding(direction) }
            }.onEnded { _ in holding = nil; remote.stopHolding() })
            .onDisappear { holding = nil; remote.stopHolding() }
    }
}
#endif
