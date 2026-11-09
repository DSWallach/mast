import SwiftUI

struct ContentView: View {
    @AppStorage("mastStyleChoice") private var styleChoice = "match"
    @AppStorage("mastAppearanceChoice") private var appearanceChoice = "system"
    private var style: MastStyle { MastStyle(id: MastStyleID(rawValue: styleChoice == "match" ? model.macAppearance.style : styleChoice) ?? .default, lightTheme: styleChoice == "match" ? model.macAppearance.lightTheme : nil, darkTheme: styleChoice == "match" ? model.macAppearance.darkTheme : nil) }
    private var chosenScheme: ColorScheme? { appearanceChoice == "light" ? .light : appearanceChoice == "dark" ? .dark : nil }
    private var effectiveScheme: ColorScheme { chosenScheme ?? scheme }
    @State private var link: CompanionLink?
    @State private var pendingExternal: URL?
    @State private var confirmExternal = false
    @StateObject private var model = Companion()
    @StateObject private var appLock = AppLock()
    @AppStorage("lockTimeoutSeconds") private var lockTimeout = 0
    @Environment(\.scenePhase) private var phase
    @Environment(\.colorScheme) private var scheme
    var body: some View { ZStack { VStack(spacing: 0) {
        if model.connection.hasPrefix("Disconnected") { OfflineBanner(text: model.connection) }
        if !model.demo { Text(model.liveStatus).font(.system(size: 10)).foregroundStyle(style.muted(effectiveScheme)).frame(maxWidth: .infinity, alignment: .trailing).padding(.horizontal, 16) }
        TabView {
            NavigationStack { InboxScreen(model: model) }.tabItem { Label("Needs you", systemImage: "tray") }
            NavigationStack { ChatsScreen(model: model) }.tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
            NavigationStack { ConnectScreen(model: model) }.tabItem { Label("Connect", systemImage: "link") }
        }.tint(style.muted(effectiveScheme)).toolbarBackground(style.surface(effectiveScheme), for: .tabBar).toolbarBackground(.visible, for: .tabBar)
    }.background(style.surface(effectiveScheme)).opacity(appLock.obscured ? 0 : 1).accessibilityHidden(appLock.obscured)
    if appLock.obscured { LockScreen(authenticating: appLock.authenticating, noPasscode: appLock.noPasscode, unlock: { appLock.unlock() }, continueWithoutPasscode: { appLock.continueWithoutPasscode() }) }
    }.fullScreenCover(item: $link) { destination in
        Group { switch destination {
        case .artifact(let id): ArtifactScreen(id: id, model: model)
        case .chat(let id): ChatLinkScreen(id: id, model: model, close: { link = nil })
        case .safari(let url): SafariScreen(url: url)
        case .mac: VStack { Text("Open on Mac"); Button("Done") { link = nil } }
        } }.confirmationDialog("Open this link in Safari?", isPresented: Binding(get: { confirmExternal && link != nil }, set: { if !$0 { confirmExternal = false } })) { Button("Open") { if let url = pendingExternal { link = .safari(url) }; pendingExternal = nil }; Button("Cancel", role: .cancel) { pendingExternal = nil } } message: { if let url = pendingExternal { Text(url.absoluteString) } }
    }.confirmationDialog("Open this link in Safari?", isPresented: Binding(get: { confirmExternal && link == nil }, set: { if !$0 { confirmExternal = false } })) { Button("Open") { if let url = pendingExternal { link = .safari(url) }; pendingExternal = nil }; Button("Cancel", role: .cancel) { pendingExternal = nil } } message: { if let url = pendingExternal { Text(url.absoluteString) } }
    .environment(\.openURL, OpenURLAction { url in
        guard let destination = companionLink(url) else { return .discarded }
        if case .safari(let external) = destination { pendingExternal = external; confirmExternal = true } else { link = destination }; return .handled
    }).environment(\.mastStyle, style).buttonStyle(MastTapTargetStyle()).preferredColorScheme(chosenScheme).onAppear { if phase == .active { appLock.foreground(timeout: TimeInterval(lockTimeout)) } }
        .onChange(of: phase) { _, value in
            switch value { case .active: appLock.foreground(timeout: TimeInterval(lockTimeout)); case .inactive: appLock.inactive(); case .background: appLock.background(); @unknown default: appLock.inactive() }
        }.onChange(of: appLock.obscured) { _, obscured in if !obscured { Task { await model.refresh() } } else { link = nil; pendingExternal = nil; confirmExternal = false } }
        .task(id: "\(appLock.obscured)-\(model.demo)-\(model.activeChatId ?? "")") { if !appLock.obscured { await model.watchEvents() } }
    }
}
struct LockScreen: View {
    @Environment(\.mastStyle) private var style
    let authenticating: Bool
    var noPasscode = false
    let unlock: () -> Void
    var continueWithoutPasscode: () -> Void = {}
    @Environment(\.colorScheme) private var scheme
    var body: some View { ZStack { style.surface(scheme).ignoresSafeArea(); VStack(spacing: 18) {
        Image(systemName: "lock.fill").font(.system(size: 28)).foregroundStyle(style.muted(scheme))
        Text("Mast is locked").font(.system(size: 26, weight: .semibold)).foregroundStyle(style.ink(scheme))
        Button(authenticating ? "Unlocking…" : "Unlock") { unlock() }.disabled(authenticating).foregroundStyle(style.muted(scheme))
        if noPasscode { Text("This device has no passcode. Set one for a private lock, or continue without it.").font(.system(size: 13)).multilineTextAlignment(.center).foregroundStyle(style.muted(scheme)); Button("Continue without lock") { continueWithoutPasscode() }.foregroundStyle(style.muted(scheme)) }
        else { Text("Use Face ID or your device passcode.").font(.system(size: 13)).foregroundStyle(style.muted(scheme)) }
    }.padding(24) } }
}

import SafariServices
import WebKit

enum CompanionLink: Identifiable {
    case artifact(String), chat(String), safari(URL), mac
    var id: String { switch self { case .artifact(let id): "artifact-\(id)"; case .chat(let id): "chat-\(id)"; case .safari(let url): "safari-\(url)"; case .mac: "mac" } }
}
func companionLink(_ url: URL) -> CompanionLink? {
    let value = url.absoluteString
    switch url.scheme?.lowercased() {
    case "mast-artifact": let id = String(value.dropFirst("mast-artifact:".count)); return id.range(of: "^[A-Za-z0-9_-]{1,128}$", options: .regularExpression) == nil ? nil : .artifact(id)
    case "mast-chat": let id = String(value.dropFirst("mast-chat:".count)); return id.range(of: "^[A-Za-z0-9_-]{1,128}$", options: .regularExpression) == nil ? nil : .chat(id)
    case "https": return url.host == nil ? nil : .safari(url)
    case "mast-open-on-mac", "file", "mast-diff": return .mac
    default: return nil
    }
}
struct SafariScreen: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context: Context) -> SFSafariViewController { SFSafariViewController(url: url) }
    func updateUIViewController(_ controller: SFSafariViewController, context: Context) {}
}
struct ArtifactScreen: View {
    let id: String
    @ObservedObject var model: Companion
    @Environment(\.dismiss) private var dismiss
    @Environment(\.mastStyle) private var style
    @Environment(\.colorScheme) private var scheme
    @State private var metadata: ArtifactMetadata?
    @State private var data: Data?
    @Environment(\.snapshotRendering) private var snapshot
    init(id: String, model: Companion, initial: (ArtifactMetadata, Data)? = nil) { self.id = id; self.model = model; _metadata = State(initialValue: initial?.0); _data = State(initialValue: initial?.1) }
    @State private var error = ""
    @State private var zoom: CGFloat = 1
    @ViewBuilder private var artifactBody: some View { Group {
        if let metadata, let data {
            if model.demo || model.canView(spaceId: metadata.spaceId) {
                switch artifactDisplayKind(metadata.kind) {
                case "markdown": HorizonScroll { MarkdownReply(text: String(decoding: data, as: UTF8.self), linkColor: style.action(scheme)).padding(22) }
                case "image": if let image = UIImage(data: data) { if snapshot { Image(uiImage: image).resizable().scaledToFit() } else { ScrollView([.horizontal, .vertical]) { Image(uiImage: image).resizable().scaledToFit().scaleEffect(zoom).gesture(MagnificationGesture().onChanged { zoom = max(1, min($0, 5)) }) } } } else { Text("Image unavailable") }
                case "html": if snapshot { Text("HTML preview").frame(maxWidth: .infinity, maxHeight: .infinity) } else { SandboxedHTML(html: String(decoding: data, as: UTF8.self)) }
                default: VStack(spacing: 12) { Text("Open on Mac"); Text(metadata.title).foregroundStyle(style.muted(scheme)) }
                }
            } else { Text("Space is unavailable or no longer shared.") }
        } else if error.isEmpty { ProgressView() } else { Text(error) }
    }.frame(maxWidth: .infinity, maxHeight: .infinity).background(style.surface(scheme)) }
    var body: some View { Group { if snapshot { artifactBody } else { NavigationStack { artifactBody.navigationTitle(metadata?.title ?? "Artifact").toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }; ToolbarItem(placement: .primaryAction) { if metadata != nil { ShareLink(item: "mast-artifact:\(id)") { Image(systemName: "square.and.arrow.up").frame(minWidth: 44, minHeight: 44).contentShape(Rectangle()) }.accessibilityLabel("Share artifact link") } } }.task { if metadata == nil { do { (metadata, data) = try await model.artifact(id) } catch { self.error = "Artifact unavailable." } } } } } } }
}
func artifactDisplayKind(_ kind: String) -> String {
    switch kind.lowercased() { case "htmlpage", "htmlfragment", "html-page", "html-fragment": "html"; case "markdown": "markdown"; case "image": "image"; default: "other" }
}
func allowedArtifactNavigation(_ url: URL) -> Bool { url.absoluteString == "about:blank" }
struct SandboxedHTML: UIViewRepresentable {
    @Environment(\.openURL) private var openURL
    let html: String
    func makeCoordinator() -> Coordinator { Coordinator(open: { openURL($0) }) }
    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration(); config.websiteDataStore = .nonPersistent(); config.defaultWebpagePreferences.allowsContentJavaScript = false
        let view = WKWebView(frame: .zero, configuration: config); view.navigationDelegate = context.coordinator
        view.scrollView.minimumZoomScale = 1; view.scrollView.maximumZoomScale = 5
        let csp = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'\">"
        let rules = #"[{"trigger":{"url-filter":".*"},"action":{"type":"block"}},{"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},{"trigger":{"url-filter":"^about:"},"action":{"type":"ignore-previous-rules"}}]"#
        WKContentRuleListStore.default().compileContentRuleList(forIdentifier: "mast-artifact-offline", encodedContentRuleList: rules) { list, _ in
            guard let list else { return }
            DispatchQueue.main.async { config.userContentController.add(list); view.loadHTMLString(csp + html, baseURL: nil) }
        }
        return view
    }
    func updateUIView(_ uiView: WKWebView, context: Context) {}
    final class Coordinator: NSObject, WKNavigationDelegate {
        let open: (URL) -> Void
        init(open: @escaping (URL) -> Void) { self.open = open }
        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            guard let url = action.request.url else { decisionHandler(.cancel); return }
            if action.navigationType == .linkActivated { decisionHandler(.cancel); open(url); return }
            guard allowedArtifactNavigation(url) else { decisionHandler(.cancel); return }; decisionHandler(.allow)
        }
    }
}

struct ChatLinkScreen: View {
    let id: String
    @ObservedObject var model: Companion
    let close: () -> Void
    @State private var chat: Chat?
    @State private var unavailable = false
    var body: some View { NavigationStack { Group { if let chat { ChatDetail(chat: chat, model: model) } else if unavailable { Text("Chat unavailable or not shared.") } else { ProgressView() } }.toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done", action: close) } }.task { do { chat = try await model.chatLink(id) } catch { unavailable = true } } } }
}
