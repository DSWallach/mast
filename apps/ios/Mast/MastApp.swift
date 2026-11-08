import SwiftUI
import Security

@main
struct MastApp: App {
    var body: some Scene { WindowGroup { ContentView() } }
}

struct ActionRequest: Identifiable, Decodable {
    let id: String, space: String, kind: String, text: String, detail: String
    let options: [String], facts: [String], links: [String]
    let origin: String
}
struct Chat: Identifiable, Decodable {
    let id: String, title: String, space: String, status: String
}
struct TranscriptMessage: Identifiable, Decodable {
    let id: String, role: String, text: String
}
private enum Demo {
    static let requests = [ActionRequest(id: "1", space: "Mast", kind: "Review", text: "Review the companion layout", detail: "Check the inbox and transcript before the next release.", options: ["Looks good", "Needs changes"], facts: ["Priority: Normal"], links: ["Review: https://example.com/review"], origin: "Companion design chat")]
    static let chats = [Chat(id: "1", title: "Companion design", space: "Mast", status: "Working"), Chat(id: "2", title: "Release notes", space: "Mast", status: "Waiting for input")]
    static let messages = [TranscriptMessage(id: "1", role: "You", text: "Prepare a compact inbox for the phone."), TranscriptMessage(id: "2", role: "Assistant", text: "The inbox is ready for review.")]
}

private enum Credential {
    static let service = "computer.mikey.mast.companion.bridge"
    static func read() -> String? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device", kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }
    static func save(_ token: String) throws {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device"]
        SecItemDelete(query as CFDictionary)
        var item = query
        item[kSecValueData as String] = Data(token.utf8)
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw BridgeError.credential }
    }
    static func clear() { SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device"] as CFDictionary) }
}
private enum BridgeError: LocalizedError {
    case address, credential, response
    var errorDescription: String? {
        switch self { case .address: return "Use HTTPS, or HTTP on simulator loopback."; case .credential: return "Could not save the device credential."; case .response: return "Bridge request failed. Check the connection and pairing." }
    }
}
private struct PairReply: Decodable { let token: String }
private struct SendReply: Decodable { let delivery: String }
private final class NoRedirect: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
@MainActor
private final class Companion: ObservableObject {
    @Published var demo = true
    @Published var address = "http://127.0.0.1:27655"
    @Published var requests = Demo.requests
    @Published var chats = Demo.chats
    @Published var connection = "Demo mode"
    @Published var pairingCode = ""
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
        if authenticated { guard let token = Credential.read() else { throw BridgeError.credential }; request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let session = URLSession(configuration: .ephemeral, delegate: NoRedirect(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { throw BridgeError.response }
        return try JSONDecoder().decode(T.self, from: data)
    }
    func pair() async {
        do {
            let reply: PairReply = try await request("/pair", body: ["code": pairingCode], authenticated: false)
            try Credential.save(reply.token); pairingCode = ""; demo = false; requests = []; chats = []; await refresh()
        } catch { connection = "Disconnected · \(error.localizedDescription)" }
    }
    func refresh() async {
        if demo { requests = Demo.requests; chats = Demo.chats; connection = "Demo mode"; return }
        do {
            async let newRequests: [ActionRequest] = request("/requests")
            async let newChats: [Chat] = request("/chats")
            (requests, chats) = try await (newRequests, newChats)
            connection = "Connected · updated now"
        } catch { connection = "Disconnected · showing stale data" }
    }
    func transcript(_ chat: Chat) async throws -> [TranscriptMessage] {
        if demo { return Demo.messages }
        return try await request("/chats/\(chat.id)/transcript")
    }
    func send(_ chat: Chat, text: String) async throws -> String {
        if demo { return "Demo only · message was not sent" }
        let result: SendReply = try await request("/chats/\(chat.id)/messages", body: ["text": text])
        if result.delivery == "simulated" { return "Fixture only · message was not sent" }
        return result.delivery == "queued" ? "Queued" : "Accepted"
    }
    func forget() { Credential.clear(); demo = true; requests = Demo.requests; chats = Demo.chats; connection = "Demo mode" }
}

private struct ContentView: View {
    @StateObject private var model = Companion()
    @Environment(\.scenePhase) private var phase
    var body: some View {
        TabView {
            NavigationStack {
                List {
                    Section { Label(model.connection, systemImage: model.connection.hasPrefix("Disconnected") ? "wifi.slash" : "circle.fill").foregroundStyle(.secondary) }
                    Section("Needs you") {
                        ForEach(model.requests) { request in
                            NavigationLink { RequestDetail(request: request) } label: {
                                VStack(alignment: .leading, spacing: 5) { Text(request.text).font(.headline); Text("\(request.space) · \(request.kind)").font(.subheadline).foregroundStyle(.secondary) }.padding(.vertical, 3)
                            }
                        }
                    }
                }.navigationTitle("Needs you").refreshable { await model.refresh() }
            }.tabItem { Label("Needs you", systemImage: "tray") }
            NavigationStack {
                List(model.chats) { chat in
                    NavigationLink { ChatDetail(chat: chat, model: model) } label: {
                        VStack(alignment: .leading, spacing: 5) { Text(chat.title).font(.headline); Text("\(chat.space) · \(chat.status)").font(.subheadline).foregroundStyle(.secondary) }.padding(.vertical, 3)
                    }
                }.navigationTitle("Chats").refreshable { await model.refresh() }
            }.tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
            NavigationStack {
                Form {
                    Section("Connection") {
                        Toggle("Demo mode", isOn: $model.demo).onChange(of: model.demo) { _, demo in if !demo { model.requests = []; model.chats = [] }; Task { await model.refresh() } }
                        TextField("Bridge URL", text: $model.address).textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                        SecureField("Pairing code", text: $model.pairingCode).textInputAutocapitalization(.never).autocorrectionDisabled()
                        Button("Pair") { Task { await model.pair() } }.disabled(model.pairingCode.isEmpty)
                        Button("Forget device credential") { model.forget() }
                        Text(model.connection).foregroundStyle(.secondary)
                    }
                    Section("Actions") { Toggle("Reply & resolve", isOn: .constant(false)).disabled(true); Text("Replies are unavailable until delivery and resolution are verified.").foregroundStyle(.secondary) }
                }.navigationTitle("Settings")
            }.tabItem { Label("Settings", systemImage: "gearshape") }
        }.tint(Color(red: 142 / 255, green: 164 / 255, blue: 184 / 255))
            .onChange(of: phase) { _, value in if value == .active { Task { await model.refresh() } } }
    }
}
private struct RequestDetail: View {
    let request: ActionRequest
    var body: some View {
        List {
            Section { Text(request.detail) }
            Section("Options") { ForEach(request.options, id: \.self) { Text($0) } }
            Section("Facts") { ForEach(request.facts, id: \.self) { Text($0) } }
            Section("Links") { ForEach(request.links, id: \.self) { Text($0).textSelection(.enabled) } }
            Section("Origin") { Text(request.origin) }
        }.navigationTitle(request.text).navigationBarTitleDisplayMode(.inline)
    }
}
private struct ChatDetail: View {
    let chat: Chat
    @ObservedObject var model: Companion
    @State private var text = ""
    @State private var notice = ""
    @State private var messages: [TranscriptMessage] = []
    @State private var sending = false
    var body: some View {
        VStack {
            List { Section(chat.status) { ForEach(messages) { message in VStack(alignment: .leading, spacing: 5) { Text(message.role).font(.caption).foregroundStyle(.secondary); Text(message.text).textSelection(.enabled) }.padding(.vertical, 4) } } }
            if !notice.isEmpty { Text(notice).font(.caption).foregroundStyle(.secondary) }
            HStack {
                TextField("Message", text: $text, axis: .vertical).textFieldStyle(.roundedBorder)
                Button("Send") {
                    let message = text; sending = true
                    Task { do { notice = try await model.send(chat, text: message); text = "" } catch { notice = "Send failed or outcome unknown. Check the chat before retrying." }; sending = false }
                }.disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }.padding()
        }.navigationTitle(chat.title).navigationBarTitleDisplayMode(.inline)
            .task { do { messages = try await model.transcript(chat) } catch { notice = "Transcript unavailable or stale." } }
    }
}
