import SwiftUI
import Security

@main
struct MastApp: App {
    var body: some Scene { WindowGroup { ContentView() } }
}

struct ActionRequest: Identifiable, Codable {
    let id: String, space: String, kind: String, text: String, detail: String
    let options: [String], facts: [String], links: [String]
    let origin: String
    var priority: String? = nil, item: String? = nil
    var createdAt: Double? = nil
}
struct Chat: Identifiable, Codable {
    let id: String, title: String, space: String, status: String
    var harness: String? = nil, model: String? = nil, lastMessageAt: String? = nil
    var isWorker: Bool? = nil, hasUserMessage: Bool? = nil, hasOpenRequest: Bool? = nil
    func visible(showWorkers: Bool) -> Bool { hasOpenRequest == true || (isWorker == true ? showWorkers : hasUserMessage != false) }
}
struct TranscriptMessage: Identifiable, Codable {
    let id: String, role: String, text: String
    var author: String? = nil
}
struct TranscriptPage: Decodable { let messages: [TranscriptMessage]; let olderRemaining: Int; let nextOffset: Int }
enum Demo {
    static let requests = [ActionRequest(id: "1", space: "Mast", kind: "Review", text: "Review the companion layout", detail: "Check the inbox and transcript before the next release.", options: ["Looks good", "Needs changes"], facts: ["Priority: Normal", "Screen: Needs you"], links: ["Review: https://example.com/review"], origin: "Companion design chat", priority: "blocking", item: "iOS companion", createdAt: Date().addingTimeInterval(-3600).timeIntervalSince1970 * 1000), ActionRequest(id: "2", space: "Product", kind: "Choose", text: "Choose the release notes for review", detail: "The draft is ready for a final pass.", options: ["Review draft"], facts: [], links: [], origin: "Release notes", item: "Launch", createdAt: Date().addingTimeInterval(-14400).timeIntervalSince1970 * 1000)]
    static let chats = [Chat(id: "1", title: "Companion design", space: "Mast", status: "working", harness: "claude-code", model: "Opus"), Chat(id: "2", title: "Release notes", space: "Mast", status: "awaitingInput", harness: "codex", model: "GPT-6"), Chat(id: "3", title: "Interface exploration", space: "Product", status: "idle", harness: "other", model: "Agent")]
    static let messages = [TranscriptMessage(id: "1", role: "You", text: "Prepare a compact inbox for the phone.", author: "user"), TranscriptMessage(id: "relay", role: "user", text: "[Message from Mast chat Coordinator (abc123)] Please check the latest build.", author: "relay"), TranscriptMessage(id: "2", role: "Assistant", text: "The inbox is ready for review.", author: "agent")]
}

protocol CredentialStore {
    func read() -> String?
    func save(_ token: String) throws
    func clear()
}
struct Credential: CredentialStore {
    private let service = "computer.mikey.mast.companion.bridge"
    func read() -> String? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device", kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }
    func save(_ token: String) throws {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device"]
        SecItemDelete(query as CFDictionary)
        var item = query
        item[kSecValueData as String] = Data(token.utf8)
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw BridgeError.credential }
    }
    func clear() { SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device"] as CFDictionary) }
}
enum BridgeError: LocalizedError {
    case address, credential, response, unauthorized
    var errorDescription: String? {
        switch self { case .address: return "Use HTTPS, or HTTP on simulator loopback."; case .credential: return "Could not save the device credential."; case .response: return "Bridge request failed. Check the connection and pairing."; case .unauthorized: return "Device credential revoked. Pair again." }
    }
}
private struct PairReply: Decodable { let token: String }
private struct SendReply: Decodable { let delivery: String }
private final class NoRedirect: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
@MainActor
final class Companion: ObservableObject {
    private let credential: any CredentialStore
    private let configuration: URLSessionConfiguration
    init(credential: any CredentialStore = Credential(), configuration: URLSessionConfiguration = .ephemeral) { self.credential = credential; self.configuration = configuration }
    @Published var demo = true
    @Published var address = "http://127.0.0.1:27655"
    @Published var requests = Demo.requests
    @Published var chats = Demo.chats
    @Published var connection = "Demo mode"
    @Published var pairingCode = ""
    @Published var showWorkers = false
    private func baseURL() throws -> URL {
        guard let components = URLComponents(string: address), let host = components.host, components.user == nil, components.password == nil, components.query == nil, components.fragment == nil, (components.path.isEmpty || components.path == "/") else { throw BridgeError.address }
        let local = host == "127.0.0.1" || host == "localhost"
        guard components.scheme == "https" || (components.scheme == "http" && local) else { throw BridgeError.address }
        guard let url = components.url else { throw BridgeError.address }; return url
    }
    private func request<T: Decodable>(_ path: String, body: [String: String]? = nil, authenticated: Bool = true) async throws -> T {
        let base = try baseURL()
        guard let url = URL(string: path, relativeTo: base)?.absoluteURL else { throw BridgeError.address }
        var request = URLRequest(url: url); request.timeoutInterval = 20
        if let body { request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type"); request.httpBody = try JSONEncoder().encode(body) }
        if authenticated { guard let token = credential.read() else { throw BridgeError.credential }; request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw BridgeError.response }
        if http.statusCode == 401 { credential.clear(); connection = "Disconnected · pair again"; throw BridgeError.unauthorized }
        guard (200..<300).contains(http.statusCode) else { throw BridgeError.response }
        return try JSONDecoder().decode(T.self, from: data)
    }
    func pair() async {
        do {
            let reply: PairReply = try await request("/pair", body: ["code": pairingCode], authenticated: false)
            try credential.save(reply.token); pairingCode = ""; demo = false; requests = []; chats = []; await refresh()
        } catch { connection = "Disconnected · \(error.localizedDescription)" }
    }
    func refresh() async {
        if demo { requests = Demo.requests; chats = Demo.chats; connection = "Demo mode"; return }
        do {
            async let newRequests: [ActionRequest] = request("/requests")
            async let newChats: [Chat] = request("/chats")
            (requests, chats) = try await (newRequests, newChats)
            connection = "Connected · updated now"
        } catch BridgeError.unauthorized { connection = "Disconnected · pair again" }
        catch { connection = "Disconnected · showing stale data" }
    }
    func transcript(_ chat: Chat, offset: Int = 0) async throws -> TranscriptPage {
        if demo { return TranscriptPage(messages: offset == 0 ? Demo.messages : [], olderRemaining: 0, nextOffset: Demo.messages.count) }
        return try await request("/chats/\(chat.id)/transcript?offset=\(offset)")
    }
    func send(_ chat: Chat, text: String) async throws -> String {
        if demo { return "Demo only · message was not sent" }
        let result: SendReply = try await request("/chats/\(chat.id)/messages", body: ["text": text])
        if result.delivery == "simulated" { return "Fixture only · message was not sent" }
        return result.delivery == "queued" ? "Queued" : "Accepted"
    }
    func forget() { credential.clear(); demo = true; requests = Demo.requests; chats = Demo.chats; connection = "Demo mode" }
}

private struct SnapshotRenderingKey: EnvironmentKey { static let defaultValue = false }
extension EnvironmentValues { var snapshotRendering: Bool { get { self[SnapshotRenderingKey.self] } set { self[SnapshotRenderingKey.self] = newValue } } }
struct HorizonScroll<Content: View>: View {
    @Environment(\.snapshotRendering) private var snapshot
    @ViewBuilder var content: Content
    var body: some View { if snapshot { content.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top) } else { ScrollView { content } } }
}
struct Horizon {
    static let slate = Color(red: 142/255, green: 164/255, blue: 184/255)
    static func surface(_ scheme: ColorScheme) -> Color { scheme == .dark ? Color(red: 24/255, green: 25/255, blue: 27/255) : .white }
    static func ink(_ scheme: ColorScheme) -> Color { scheme == .dark ? .white : Color(red: 17/255, green: 17/255, blue: 17/255) }
    static func muted(_ scheme: ColorScheme) -> Color { scheme == .dark ? slate : Color(red: 84/255, green: 105/255, blue: 123/255) }
    static func provider(_ chat: Chat, _ scheme: ColorScheme) -> Color {
        guard let model = chat.model, !model.trimmingCharacters(in: .whitespaces).isEmpty else { return muted(scheme) }
        let harness = (chat.harness ?? "").lowercased()
        if harness.contains("claude") { return scheme == .dark ? Color(red: 1, green: 184/255, blue: 0) : Color(red: 140/255, green: 96/255, blue: 0) }
        if harness.contains("codex") { return scheme == .dark ? Color(red: 29/255, green: 203/255, blue: 67/255) : Color(red: 21/255, green: 113/255, blue: 46/255) }
        return scheme == .dark ? Color(red: 11/255, green: 167/255, blue: 1) : Color(red: 0, green: 103/255, blue: 160/255)
    }
    static func relative(_ value: Double?) -> String {
        guard let value, value > 0 else { return "" }
        return Date(timeIntervalSince1970: value / 1000).formatted(.relative(presentation: .named))
    }
    static func relativeISO(_ value: String?) -> String {
        guard let value else { return "" }
        let parser = ISO8601DateFormatter(); parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = parser.date(from: value) ?? ISO8601DateFormatter().date(from: value)
        return date?.formatted(.relative(presentation: .named)) ?? ""
    }
}
struct HorizonHeader: View {
    let title: String
    var count: Int? = nil
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(alignment: .firstTextBaseline, spacing: 10) { Text(title).font(.system(size: 34, weight: .bold)); if let count { Text("\(count)").font(.system(size: 17, weight: .medium)).foregroundStyle(Horizon.muted(scheme)) }; Spacer() }.foregroundStyle(Horizon.ink(scheme)) }
}
struct OfflineBanner: View {
    let text: String
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(spacing: 8) { Image(systemName: "wifi.slash"); Text(text).lineLimit(2); Spacer() }.font(.system(size: 13, weight: .medium)).foregroundStyle(scheme == .dark ? Color.white : Horizon.muted(scheme)).padding(.horizontal, 20).padding(.vertical, 10).background(scheme == .dark ? Color(red: 41/255, green: 52/255, blue: 60/255) : Color(red: 234/255, green: 240/255, blue: 244/255)) }
}
struct InboxScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    var body: some View {
        HorizonScroll { VStack(alignment: .leading, spacing: 18) {
            HorizonHeader(title: "Needs you", count: model.requests.count)
            if model.demo { Text("DEMO MODE").font(.system(size: 10, weight: .semibold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)) }
            ForEach(model.requests.sorted { ($0.priority == "blocking" ? 0 : 1) < ($1.priority == "blocking" ? 0 : 1) }) { request in
                if snapshot { RequestCard(request: request) } else { NavigationLink { RequestDetail(request: request) } label: { RequestCard(request: request) }.buttonStyle(.plain) }
            }
            if model.requests.isEmpty { Text("Nothing needs your attention.").foregroundStyle(Horizon.muted(scheme)).padding(.top, 20) }
        }.padding(.horizontal, 22).padding(.top, 24).padding(.bottom, 30) }.refreshable { await model.refresh() }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar)
    }
}
struct RequestCard: View {
    let request: ActionRequest
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 12) {
        HStack { Text(request.kind.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.4); Spacer(); if request.priority == "blocking" { Text("BLOCKING").font(.system(size: 10, weight: .semibold)).tracking(0.8) } }.foregroundStyle(Horizon.muted(scheme))
        Text(request.text).font(.system(size: 19, weight: .semibold)).foregroundStyle(Horizon.ink(scheme)).multilineTextAlignment(.leading)
        HStack { Text([request.space, request.item].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ›  ")); Spacer(); Text(Horizon.relative(request.createdAt)) }.font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
    }.padding(19).frame(maxWidth: .infinity, alignment: .leading).background(Horizon.slate.opacity(scheme == .dark ? 0.13 : 0.10), in: RoundedRectangle(cornerRadius: 18)) }
}
struct RequestDetail: View {
    let request: ActionRequest
    @Environment(\.colorScheme) private var scheme
    private func heading(_ text: String) -> some View { Text(text.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)) }
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        Text(request.kind.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme))
        Text(request.text).font(.system(size: 30, weight: .bold)).fixedSize(horizontal: false, vertical: true)
        Text([request.space, request.item].compactMap { $0 }.joined(separator: "  ›  ")).font(.system(size: 14)).foregroundStyle(Horizon.muted(scheme))
        if !request.detail.isEmpty { Text(request.detail).font(.system(size: 17)).lineSpacing(4) }
        if !request.options.isEmpty { VStack(alignment: .leading, spacing: 10) { heading("Options"); ForEach(request.options, id: \.self) { Text($0).font(.system(size: 14, weight: .medium)).padding(.horizontal, 14).padding(.vertical, 9).background(Horizon.slate.opacity(0.14), in: Capsule()) } } }
        if !request.facts.isEmpty { VStack(alignment: .leading, spacing: 12) { heading("Facts"); ForEach(request.facts, id: \.self) { fact in let parts = fact.split(separator: ":", maxSplits: 1).map(String.init); HStack(alignment: .top) { Text(parts[0]).foregroundStyle(Horizon.muted(scheme)); Spacer(); if parts.count > 1 { Text(parts[1].trimmingCharacters(in: .whitespaces)).multilineTextAlignment(.trailing) } }.font(.system(size: 14)) } } }
        if !request.links.isEmpty { VStack(alignment: .leading, spacing: 10) { heading("Links"); ForEach(request.links, id: \.self) { Text($0).font(.system(size: 14)).textSelection(.enabled) } } }
        VStack(alignment: .leading, spacing: 8) { heading("Origin"); Text(request.origin).font(.system(size: 14)) }
        Text("Reply & resolve is unavailable until delivery is verified.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)).padding(.top, 8)
    }.foregroundStyle(Horizon.ink(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(22) }.background(Horizon.surface(scheme)).navigationBarTitleDisplayMode(.inline)
    }
}
struct ChatsScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    private func group(_ chat: Chat) -> String { let status = chat.status.lowercased(); return status.hasPrefix("working") ? "Running" : (status.contains("awaiting") || status.contains("waiting") ? "Waiting" : "Idle") }
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 25) {
        HorizonHeader(title: "Chats")
        ForEach(["Running", "Waiting", "Idle"], id: \.self) { section in
            let chats = model.chats.filter { chat in group(chat) == section && chat.visible(showWorkers: model.showWorkers) }
            if !chats.isEmpty { VStack(alignment: .leading, spacing: 12) {
                Text(section.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme))
                ForEach(chats) { chat in if snapshot { ChatRow(chat: chat) } else { NavigationLink { ChatDetail(chat: chat, model: model) } label: { ChatRow(chat: chat) }.buttonStyle(.plain) } }
            } }
        }
    }.padding(.horizontal, 22).padding(.top, 24).padding(.bottom, 30) }.refreshable { await model.refresh() }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar) }
}
struct ChatRow: View {
    let chat: Chat
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(alignment: .top, spacing: 13) {
        Circle().fill(Horizon.provider(chat, scheme)).frame(width: 9, height: 9).padding(.top, 7)
        VStack(alignment: .leading, spacing: 6) { Text(chat.title).font(.system(size: 16, weight: .semibold)).foregroundStyle(Horizon.ink(scheme)); Text([chat.model, statusLabel].compactMap { $0 }.joined(separator: "  ·  ")).font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
        Spacer(minLength: 4)
        Text(Horizon.relativeISO(chat.lastMessageAt)).font(.system(size: 12)).foregroundStyle(Horizon.muted(scheme))
    }.padding(.vertical, 8).contentShape(Rectangle()) }
    private var statusLabel: String { let status = chat.status.lowercased(); return status.contains("stale") ? "Offline" : (status.hasPrefix("working") ? "Running" : (status.contains("awaiting") || status.contains("waiting") ? "Waiting" : (status == "idle" ? "Idle" : chat.status))) }
}
struct TranscriptContent: View {
    let messages: [TranscriptMessage]
    let chat: Chat
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 24) { ForEach(messages) { message in
        if message.author == "system" || message.author == "relay" {
            SystemMessageRow(message: message)
        } else if message.author == "user" || (message.author == nil && (message.role.lowercased() == "user" || message.role.lowercased() == "you")) {
            HStack { Spacer(minLength: 45); Text(message.text).font(.system(size: 16)).padding(.horizontal, 17).padding(.vertical, 13).background(Horizon.slate.opacity(scheme == .dark ? 0.18 : 0.14), in: RoundedRectangle(cornerRadius: 19)) }
        } else {
            VStack(alignment: .leading, spacing: 9) { Text((chat.model ?? "ASSISTANT").uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.2); Text(message.text).font(.system(size: 17, design: .serif)).lineSpacing(5).textSelection(.enabled) }.foregroundStyle(Horizon.provider(chat, scheme)).frame(maxWidth: .infinity, alignment: .leading)
        }
    } }.foregroundStyle(Horizon.ink(scheme)) }
}
struct SystemMessageRow: View {
    let message: TranscriptMessage
    @State private var expanded = false
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Button { expanded.toggle() } label: {
            VStack(alignment: .leading, spacing: 5) {
                Text(message.author == "relay" ? "FROM COORDINATOR" : (message.text.hasPrefix("Worker updates:") ? "WORKER UPDATE" : "MAST"))
                    .font(.system(size: 10, weight: .bold)).tracking(1)
                Text(message.text).font(.system(size: 13)).lineLimit(expanded ? nil : 1).multilineTextAlignment(.leading)
            }.foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(13).background(Horizon.slate.opacity(0.09), in: RoundedRectangle(cornerRadius: 14))
        }.buttonStyle(.plain)
    }
}
struct ChatDetail: View {
    let chat: Chat
    @ObservedObject var model: Companion
    @State private var text = ""
    @State private var notice = ""
    @State private var messages: [TranscriptMessage]
    @State private var sending = false
    @State private var olderRemaining = 0
    @State private var nextOffset = 0
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    init(chat: Chat, model: Companion, initialMessages: [TranscriptMessage] = []) {
        self.chat = chat; self.model = model; _messages = State(initialValue: initialMessages)
    }
    var body: some View { VStack(spacing: 0) {
        ScrollViewReader { proxy in
            HorizonScroll { VStack(alignment: .leading, spacing: 20) {
                if olderRemaining > 0 { Button("Load earlier") { Task { let first = messages.first?.id; do { let page = try await model.transcript(chat, offset: nextOffset); messages = page.messages + messages; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset; if let first { proxy.scrollTo(first, anchor: .top) } } catch { notice = "Earlier messages unavailable." } } }.font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
                TranscriptContent(messages: messages, chat: chat)
                Color.clear.frame(height: 1).id("transcript-bottom")
            }.padding(22).frame(maxWidth: .infinity, alignment: .leading) }
            .task { do { let page = try await model.transcript(chat); messages = page.messages; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset; DispatchQueue.main.async { proxy.scrollTo("transcript-bottom", anchor: .bottom) } } catch { notice = "Transcript unavailable or stale." } }
            .onChange(of: sending) { _, sending in if !sending { Task { if let page = try? await model.transcript(chat) { messages = page.messages; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset; DispatchQueue.main.async { proxy.scrollTo("transcript-bottom", anchor: .bottom) } } } } }
        }
        VStack(alignment: .leading, spacing: 8) {
            if !notice.isEmpty { Text(notice).font(.system(size: 12)).foregroundStyle(Horizon.muted(scheme)) }
            HStack(alignment: .bottom, spacing: 12) {
                if snapshot { Text("Message").font(.system(size: 16)).foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Message", text: $text, axis: .vertical).font(.system(size: 16)).lineLimit(1...5) }
                Button("SEND") { let message = text; sending = true; Task { do { notice = try await model.send(chat, text: message); text = "" } catch { notice = "Send failed or outcome unknown. Check the chat before retrying." }; sending = false } }.font(.system(size: 11, weight: .bold)).tracking(1).foregroundStyle(Horizon.muted(scheme)).disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }.padding(16).background(Horizon.slate.opacity(scheme == .dark ? 0.15 : 0.12), in: RoundedRectangle(cornerRadius: 19))
        }.padding(.horizontal, 18).padding(.bottom, 12).padding(.top, 8)
    }.background(Horizon.surface(scheme)).navigationTitle(chat.title).navigationBarTitleDisplayMode(.inline) }
}
struct ConnectScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        HorizonHeader(title: "Connect")
        Text("Connect to Mast on your Mac to see your chats and requests.").font(.system(size: 16)).lineSpacing(4).foregroundStyle(Horizon.muted(scheme))
        VStack(alignment: .leading, spacing: 9) { Text("BRIDGE URL").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); Group { if snapshot { Text(model.address).frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Bridge URL", text: $model.address).textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL) } }.padding(16).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 15)) }
        VStack(alignment: .leading, spacing: 9) { Text("PAIRING CODE").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); Group { if snapshot { Text("Enter code from your Mac").foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading) } else { SecureField("Enter code from your Mac", text: $model.pairingCode).textInputAutocapitalization(.never).autocorrectionDisabled() } }.padding(16).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 15)) }
        Button("Connect") { Task { await model.pair() } }.font(.system(size: 15, weight: .semibold)).foregroundStyle(scheme == .dark ? .black : .white).frame(maxWidth: .infinity).padding(15).background(Horizon.muted(scheme), in: RoundedRectangle(cornerRadius: 15)).disabled(model.pairingCode.isEmpty)
        HStack { Circle().fill(model.connection.hasPrefix("Connected") ? Color.green : Horizon.slate).frame(width: 7, height: 7); Text(model.connection); Spacer(); Button("Disconnect") { model.forget() } }.font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
        if snapshot { HStack { Text("Demo mode"); Spacer(); Text("On").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)); HStack { Text("Show worker chats"); Spacer(); Text("Off").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)) }
        else { Toggle("Demo mode", isOn: $model.demo).tint(Horizon.muted(scheme)).onChange(of: model.demo) { _, demo in if !demo { model.requests = []; model.chats = [] }; Task { await model.refresh() } }.font(.system(size: 14)); Toggle("Show worker chats", isOn: $model.showWorkers).tint(Horizon.muted(scheme)).font(.system(size: 14)) }
        Text("Reply & resolve is unavailable until delivery is verified.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
    }.foregroundStyle(Horizon.ink(scheme)).padding(.horizontal, 22).padding(.top, 24) }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar) }
}
struct ContentView: View {
    @StateObject private var model = Companion()
    @Environment(\.scenePhase) private var phase
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(spacing: 0) {
        if model.connection.hasPrefix("Disconnected") { OfflineBanner(text: model.connection) }
        TabView {
            NavigationStack { InboxScreen(model: model) }.tabItem { Label("Needs you", systemImage: "tray") }
            NavigationStack { ChatsScreen(model: model) }.tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
            NavigationStack { ConnectScreen(model: model) }.tabItem { Label("Connect", systemImage: "link") }
        }.tint(Horizon.muted(scheme)).toolbarBackground(Horizon.surface(scheme), for: .tabBar).toolbarBackground(.visible, for: .tabBar)
    }.background(Horizon.surface(scheme)).onChange(of: phase) { _, value in if value == .active { Task { await model.refresh() } } } }
}
