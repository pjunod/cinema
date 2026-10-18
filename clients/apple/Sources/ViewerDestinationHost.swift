import SwiftUI
import UIKit

/// The presentation boundary is independent of chrome. Each visited root gets
/// one hosting controller for this profile's lifetime, including implicit pushes
/// and sheet presenters. Geometry changes only alter frames, never containment.
@MainActor
final class ViewerDestinationController: UIViewController {
    private(set) var hosts: [HomeTab: UIHostingController<AnyView>] = [:]
    private(set) var activeTab: HomeTab?
    private(set) var chrome: UIHostingController<AnyView>?
    private var header: UIHostingController<AnyView>?
    var layout: ViewerLayout = .standard
    var expanded = false
    var accessibilityNavigation = false
    var makeRoot: ((HomeTab) -> AnyView)?

    override func loadView() { view = UIView(); view.backgroundColor = .clear }
    override var shouldAutomaticallyForwardAppearanceMethods: Bool { false }
    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        activeHost?.beginAppearanceTransition(true, animated: animated)
        chrome?.beginAppearanceTransition(true, animated: animated)
        header?.beginAppearanceTransition(true, animated: animated)
    }
    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        activeHost?.endAppearanceTransition(); chrome?.endAppearanceTransition(); header?.endAppearanceTransition()
    }
    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        activeHost?.beginAppearanceTransition(false, animated: animated)
        chrome?.beginAppearanceTransition(false, animated: animated)
        header?.beginAppearanceTransition(false, animated: animated)
    }
    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        activeHost?.endAppearanceTransition(); chrome?.endAppearanceTransition(); header?.endAppearanceTransition()
    }
    private var activeHost: UIHostingController<AnyView>? { activeTab.flatMap { hosts[$0] } }

    func select(_ tab: HomeTab) {
        guard tab != activeTab else { return }
        let appearing = view.window != nil
        if let old = activeHost {
            if appearing { old.beginAppearanceTransition(false, animated: false) }
            old.willMove(toParent: nil)
            old.view.removeFromSuperview()
            old.removeFromParent()
            if appearing { old.endAppearanceTransition() }
        }
        let host: UIHostingController<AnyView>
        if let existing = hosts[tab] { host = existing }
        else {
            guard let root = makeRoot?(tab) else { return }
            host = UIHostingController(rootView: root)
            host.view.backgroundColor = .clear
            hosts[tab] = host
        }
        activeTab = tab
        addChild(host)
        if appearing { host.beginAppearanceTransition(true, animated: false) }
        view.insertSubview(host.view, at: 0)
        host.didMove(toParent: self)
        if appearing { host.endAppearanceTransition() }
        view.setNeedsLayout()
    }

    func updateChrome(_ content: AnyView, header headerContent: AnyView) {
        if let chrome { chrome.rootView = content }
        else { chrome = install(content) }
        if let header { header.rootView = headerContent }
        else { header = install(headerContent) }
        view.setNeedsLayout()
    }
    private func install(_ content: AnyView) -> UIHostingController<AnyView> {
        let host = UIHostingController(rootView: content)
        host.view.backgroundColor = .clear
        addChild(host); view.addSubview(host.view); host.didMove(toParent: self)
        return host
    }
    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        let bounds = view.bounds.inset(by: view.safeAreaInsets)
        #if os(tvOS)
        let rail = layout == .meridian
        let railWidth: CGFloat = expanded ? 232 : 88
        let barHeight: CGFloat = 80
        #else
        let rail = bounds.width >= 600
        let railWidth: CGFloat = layout == .standard ? 288 : (bounds.width >= 1100 ? 232 : 88)
        let barHeight: CGFloat = accessibilityNavigation ? 96 : 72
        #endif
        header?.view.isHidden = !rail || layout == .standard
        header?.view.isUserInteractionEnabled = !expanded
        header?.view.accessibilityElementsHidden = expanded
        if rail {
            chrome?.view.frame = CGRect(x: bounds.minX, y: bounds.minY, width: railWidth, height: bounds.height)
            let headerHeight: CGFloat = layout == .meridian ? 64 : 0
            header?.view.frame = CGRect(x: bounds.minX + railWidth, y: bounds.minY, width: bounds.width - railWidth, height: headerHeight)
            activeHost?.view.frame = CGRect(x: bounds.minX + railWidth, y: bounds.minY + headerHeight, width: bounds.width - railWidth, height: bounds.height - headerHeight)
        } else {
            #if os(tvOS)
            chrome?.view.frame = CGRect(x: bounds.minX, y: bounds.minY, width: bounds.width, height: barHeight)
            activeHost?.view.frame = CGRect(x: bounds.minX, y: bounds.minY + barHeight, width: bounds.width, height: bounds.height - barHeight)
            #else
            chrome?.view.frame = CGRect(x: bounds.minX, y: bounds.maxY - barHeight, width: bounds.width, height: barHeight)
            activeHost?.view.frame = CGRect(x: bounds.minX, y: bounds.minY, width: bounds.width, height: bounds.height - barHeight)
            #endif
            header?.view.frame = .zero
        }
    }
    override var preferredFocusEnvironments: [UIFocusEnvironment] {
        if expanded, let chrome { return [chrome] }
        return activeHost.map { [$0] } ?? []
    }
}

struct ViewerDestinationHost: UIViewControllerRepresentable {
    let selection: HomeTab
    let layout: ViewerLayout
    let expanded: Bool
    let accessibilityNavigation: Bool
    let panelDismissed: () -> Void
    let root: (HomeTab) -> AnyView
    let chrome: AnyView
    let header: AnyView
    func makeUIViewController(context: Context) -> ViewerDestinationController {
        let controller = ViewerDestinationController()
        controller.makeRoot = root
        return controller
    }
    func updateUIViewController(_ controller: ViewerDestinationController, context: Context) {
        controller.layout = layout
        controller.accessibilityNavigation = accessibilityNavigation
        let changedExpansion = controller.expanded != expanded
        controller.expanded = expanded
        controller.hosts[selection]?.view.isUserInteractionEnabled = !expanded
        controller.hosts[selection]?.view.accessibilityElementsHidden = expanded
        controller.makeRoot = root
        for (tab, host) in controller.hosts { host.rootView = root(tab) }
        controller.select(selection)
        controller.hosts[selection]?.view.isUserInteractionEnabled = !expanded
        controller.hosts[selection]?.view.accessibilityElementsHidden = expanded
        controller.updateChrome(chrome, header: header)
        if changedExpansion && !expanded {
            Task { @MainActor in
                await Task.yield()
                panelDismissed()
            }
        }
        if changedExpansion { controller.setNeedsFocusUpdate() }
    }
}

extension HomeTab {
    var title: String {
        switch self {
        case .home: return "Home"
        case .libraries: return "Libraries"
        case .liveTv: return "Live TV"
        case .recordings: return "Recordings"
        case .libraryChannels: return "Channels"
        case .search: return "Search"
        case .downloads: return "Downloads"
        case .settings: return "Settings"
        }
    }
    var symbol: String {
        switch self {
        case .home: return "house"
        case .libraries: return "rectangle.stack"
        case .liveTv: return "tv"
        case .recordings: return "record.circle"
        case .libraryChannels: return "play.rectangle.on.rectangle"
        case .search: return "magnifyingglass"
        case .downloads: return "arrow.down.circle"
        case .settings: return "gearshape"
        }
    }
    static var viewerTabs: [HomeTab] {
        #if os(iOS)
        return [.home, .libraries, .liveTv, .recordings, .libraryChannels, .search, .downloads, .settings]
        #else
        return [.home, .libraries, .liveTv, .recordings, .libraryChannels, .search, .settings]
        #endif
    }
}

struct ViewerNavigationChrome: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let expanded: Bool
    let recordingsAttention: Int
    let togglePanel: () -> Void
    let select: (HomeTab) -> Void
    let selectSource: (LibraryCollection) -> Void

    private var attention: Int { model.features.recordings ? max(0, recordingsAttention) : 0 }
    private func menuTitle(_ tab: HomeTab) -> String {
        tab == .recordings && attention > 0 ? "Recordings · \(attention) need attention" : tab.title
    }
    private var tabs: [HomeTab] { HomeTab.viewerTabs.filter { $0.isVisible(model.features) } }
    var body: some View {
        GeometryReader { geometry in
            #if os(tvOS)
            let rail = model.layout == .meridian
            #else
            let rail = geometry.size.height > geometry.size.width
            #endif
            Group {
                if rail {
                    if model.layout == .standard {
                        List {
                            Section("Cinema") {
                                ForEach(tabs, id: \.self) { tab in
                                    Button { select(tab) } label: {
                                        HStack {
                                            Label(tab.title, systemImage: tab.symbol)
                                            Spacer()
                                            if tab == .recordings && attention > 0 { RecordingAttentionBadge(count: attention).frame(width: 28, height: 22) }
                                        }.frame(minHeight: 44)
                                    }
                                    .accessibilityIdentifier("viewer-tab-" + tab.title)
                                    .accessibilityAddTraits(navigation.selectedTab == tab ? .isSelected : [])
                                    .listRowBackground(navigation.selectedTab == tab ? Palette.accent.opacity(0.15) : Palette.surface)
                                }
                            }
                        }
                        #if os(iOS)
                        .listStyle(.sidebar)
                        .scrollContentBackground(.hidden)
                        #endif
                        .accessibilityIdentifier("standard-native-sidebar")
                    } else {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 12) {
                            #if os(tvOS)
                            Button(action: togglePanel) {
                                Label(expanded ? "Close navigation" : "Open navigation", systemImage: "sidebar.left")
                                    .labelStyle(.iconOnly).frame(minWidth: 48, minHeight: 48)
                            }
                            .accessibilityIdentifier("meridian-navigation-toggle")
                            .remoteControl(expanded ? "choice:close" : RemoteNavigationCoordinator.viewerNavigationOpener,
                                label: expanded ? "Close navigation" : "Open navigation", activate: togglePanel)
                            .environment(\.remoteNavigationScope, expanded ? RemoteNavigationCoordinator.viewerNavigationScope : navigation.rootScope)
                            .id(expanded ? RemoteNavigationCoordinator.viewerNavigationScope : navigation.rootScope)
                            #else
                            Text(geometry.size.width > 120 ? "cinema" : "C")
                                .font(.title2.bold()).foregroundStyle(Palette.accent).padding(.vertical, 16)
                            #endif
                            ForEach(tabs, id: \.self) { tab in
                                if expanded {
                                    destination(tab, labels: geometry.size.width > 120)
                                        .remoteControl("choice:tab:" + tab.title, label: tab.title) { select(tab) }
                                } else { destination(tab, labels: geometry.size.width > 120) }
                            }
                            if geometry.size.width > 120 {
                                Divider().overlay(Palette.outline)
                                Text("LIBRARIES").font(.caption.bold()).foregroundStyle(Palette.muted)
                                ForEach(model.libraryCollections(grouping: .share)) { source in
                                    Button { selectSource(source) } label: {
                                        Label(source.title, systemImage: "folder").lineLimit(2)
                                            .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                                    }
                                    .accessibilityIdentifier("meridian-source-" + source.id)
                                    .remoteControl("choice:source:" + source.id, label: source.title) { selectSource(source) }
                                }
                            }
                        }.padding(12)
                    }
                    }
                } else {
                    HStack(spacing: 4) {
                        if model.layout == .meridian {
                            ForEach([HomeTab.home, .libraries, .search], id: \.self) { destination($0, labels: true, compact: true) }
                            Menu {
                                ForEach(tabs.filter { ![.home, .libraries, .search].contains($0) }, id: \.self) { tab in
                                    Button { select(tab) } label: { Label(menuTitle(tab), systemImage: tab.symbol) }
                                        .accessibilityLabel(menuTitle(tab))
                                }
                            } label: {
                                VStack(spacing: 4) {
                                    Image(systemName: "ellipsis").font(.system(size: 20))
                                    Text("More").font(.caption2.weight(.semibold)).lineLimit(1)
                                }
                                .frame(maxWidth: .infinity, minHeight: 56)
                            }
                            .overlay(alignment: .topTrailing) {
                                if attention > 0 { RecordingAttentionBadge(count: attention).frame(width: 24, height: 20) }
                            }
                            .accessibilityLabel(attention > 0 ? "More, \(attention) recordings need attention" : "More")
                            .accessibilityIdentifier("meridian-more")
                        } else {
                            #if os(tvOS)
                            ForEach(tabs, id: \.self) { destination($0, labels: true) }
                            #else
                            ForEach(tabs.prefix(4), id: \.self) { destination($0, labels: true) }
                            Menu {
                                ForEach(Array(tabs.dropFirst(4)), id: \.self) { tab in
                                    Button { select(tab) } label: { Label(menuTitle(tab), systemImage: tab.symbol) }
                                        .accessibilityLabel(menuTitle(tab))
                                }
                            } label: { Label("More", systemImage: "ellipsis").frame(minWidth: 48, minHeight: 48) }
                            #endif
                        }
                    }.padding(.horizontal, 8)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Palette.surface)
            .foregroundStyle(Palette.onBg)
            .dynamicTypeSize(...DynamicTypeSize.xxxLarge)
            .environment(\.remoteNavigationScope, RemoteNavigationCoordinator.viewerNavigationScope)
            #if os(tvOS)
            .background {
                if expanded {
                    RemoteChoiceFocusReadiness {
                        enterPanel()
                    }
                }
            }
            .onChange(of: model.features) { _, _ in if expanded { enterPanel() } }
            .onChange(of: model.libraryCollections(grouping: .share)) { _, _ in if expanded { enterPanel() } }
            .onExitCommand { if expanded { togglePanel() } }
            #endif
        }
    }
    private func enterPanel() {
        navigation.enterChoicePanel(scope: RemoteNavigationCoordinator.viewerNavigationScope,
            keys: tabs.map { "choice:tab:" + $0.title } + model.libraryCollections(grouping: .share).map { "choice:source:" + $0.id } + ["choice:close"],
            selected: "choice:tab:" + navigation.selectedTab.title)
    }
    private func destination(_ tab: HomeTab, labels: Bool, compact: Bool = false) -> some View {
        Button { select(tab) } label: {
            Group {
                if compact {
                    VStack(spacing: 4) {
                        Image(systemName: tab.symbol).font(.system(size: 20))
                        Text(tab.title).font(.caption2.weight(.semibold)).lineLimit(1).minimumScaleFactor(0.5)
                    }
                } else if labels {
                    Label(tab.title, systemImage: tab.symbol).lineLimit(1)
                } else { Image(systemName: tab.symbol) }
            }
            #if os(tvOS)
            .font(.system(size: 22, weight: .semibold))
            #else
            .font(.callout.weight(.semibold))
            #endif
            .frame(maxWidth: .infinity, minHeight: compact ? 56 : 48, alignment: !compact && labels && model.layout == .meridian ? .leading : .center)
            .padding(.horizontal, compact ? 4 : 8)
            .foregroundStyle(navigation.selectedTab == tab ? Palette.selectedInk : Palette.onBg)
            .background(navigation.selectedTab == tab ? Palette.selectedFill : Palette.surface, in: RoundedRectangle(cornerRadius: 14))
            .overlay(alignment: .topTrailing) {
                if tab == .recordings && attention > 0 {
                    RecordingAttentionBadge(count: attention).frame(width: 24, height: 20)
                }
            }
        }
        .frame(maxWidth: compact ? .infinity : nil)
        .accessibilityLabel(tab.title)
        .accessibilityIdentifier("viewer-tab-" + tab.title)
        .accessibilityAddTraits(navigation.selectedTab == tab ? .isSelected : [])
        #if os(tvOS)
        .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
        #else
        .buttonStyle(.plain)
        #endif
    }
}

struct ViewerNavigationHeader: View {
    let search: () -> Void
    let settings: () -> Void
    var body: some View {
        HStack {
            Button(action: search) { Label("Search your library", systemImage: "magnifyingglass") }
                .accessibilityIdentifier("meridian-header-search")
            Spacer()
            Button(action: settings) { Label("Account & Appearance", systemImage: "person.crop.circle") }
                .accessibilityIdentifier("meridian-header-account")
        }
        .padding(.horizontal, 24).frame(maxHeight: .infinity)
        .foregroundStyle(Palette.onBg).background(Palette.surface)
    }
}

/// A native badge keeps its visible count and VoiceOver announcement together
/// on the rail, native sidebar and compact More opener.
struct RecordingAttentionBadge: UIViewRepresentable {
    let count: Int
    func makeUIView(context: Context) -> UILabel {
        let label = UILabel()
        label.textAlignment = .center
        label.font = .preferredFont(forTextStyle: .caption2)
        label.adjustsFontForContentSizeCategory = true
        label.layer.cornerRadius = 10
        label.clipsToBounds = true
        label.accessibilityIdentifier = "viewer-recording-attention-badge"
        return label
    }
    func updateUIView(_ label: UILabel, context: Context) {
        label.text = String(count)
        label.accessibilityLabel = "\(count) recordings need attention"
        label.backgroundColor = UIColor(Palette.accent)
        label.textColor = UIColor(Palette.buttonInk)
    }
}
