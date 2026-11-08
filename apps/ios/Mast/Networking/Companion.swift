import SwiftUI
import Security
import UIKit

struct BoundCredential: Codable, Equatable { let origin: String, token: String }
protocol CredentialStore {
    func read() -> BoundCredential?
    func save(_ credential: BoundCredential) throws
    func clear()
}
protocol DecisionStore {
    func read(_ requestId: String) throws -> PendingDecision?
    func save(_ requestId: String, decision: PendingDecision) throws
}
struct KeychainDecisionStore: DecisionStore {
    private let service = "computer.mikey.mast.companion.decisions"
    private func query(_ id: String) -> [String: Any] { [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: id] }
    func read(_ requestId: String) throws -> PendingDecision? {
        var attributes = query(requestId); attributes[kSecReturnData as String] = true; attributes[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?; let status = SecItemCopyMatching(attributes as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw BridgeError.credential }
        guard SecItemUpdate(query(requestId) as CFDictionary, [kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly] as CFDictionary) == errSecSuccess else { throw BridgeError.credential }
        return try JSONDecoder().decode(PendingDecision.self, from: data)
    }
    func save(_ requestId: String, decision: PendingDecision) throws {
        let data = try JSONEncoder().encode(decision); let attributes = query(requestId)
        let status = SecItemUpdate(attributes as CFDictionary, [kSecValueData as String: data, kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly] as CFDictionary)
        if status == errSecItemNotFound {
            var item = attributes; item[kSecValueData as String] = data; item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw BridgeError.credential }
        } else if status != errSecSuccess { throw BridgeError.credential }
    }
}
struct Credential: CredentialStore {
    private let service = "computer.mikey.mast.companion.bridge"
    func read() -> BoundCredential? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device", kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        let identity: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device"]
        guard SecItemUpdate(identity as CFDictionary, [kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly] as CFDictionary) == errSecSuccess else { return nil }
        return try? JSONDecoder().decode(BoundCredential.self, from: data)
    }
    func save(_ credential: BoundCredential) throws {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "device"]
        SecItemDelete(query as CFDictionary)
        var item = query
        item[kSecValueData as String] = try JSONEncoder().encode(credential)
        item[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
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
struct ArtifactMetadata: Decodable { let id: String, title: String, kind: String, spaceId: String; let revision: Int }
private struct PairReply: Decodable { let token: String }
private struct SendReply: Decodable { let delivery: String }
private struct StopReply: Decodable { let status: String }
private final class NoRedirect: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
@MainActor
final class Companion: ObservableObject {
    private let credential: any CredentialStore
    private let decisions: any DecisionStore
    private let configuration: URLSessionConfiguration
    private let actionAuthenticator: any DeviceAuthenticator
    private let actionAllowed: @MainActor () -> Bool
    init(credential: any CredentialStore = Credential(), decisions: any DecisionStore = KeychainDecisionStore(), configuration: URLSessionConfiguration = .ephemeral, actionAuthenticator: any DeviceAuthenticator = LocalDeviceAuthenticator(), actionAllowed: @escaping @MainActor () -> Bool = { UIApplication.shared.applicationState == .active }) { self.actionAllowed = actionAllowed; self.actionAuthenticator = actionAuthenticator; self.credential = credential; self.decisions = decisions; self.configuration = configuration; if let stored = credential.read() { address = stored.origin; isPaired = true } }
    @Published var demo = true
    @Published private(set) var isPaired = false
    @Published var address = "http://127.0.0.1:27655"
    @Published var requests = Demo.requests
    @Published var chats = Demo.chats
    @Published var connection = "Demo mode"
    @Published var pairingCode = ""
    @Published var showWorkers = false
    @Published var replyEnabled = UserDefaults.standard.object(forKey: "replyAndResolveEnabled") as? Bool ?? true {
        didSet { UserDefaults.standard.set(replyEnabled, forKey: "replyAndResolveEnabled") }
    }
    @Published var isLoading = false
    @Published var hasLoaded = false
    @Published var lastUpdated: Date?
    @Published var macAppearance = MacAppearance(style: "default", layout: "minimal", lightTheme: "mast-light", darkTheme: "mast-dark")
    @Published var activeChatId: String?
    @Published var transcriptRevision = 0
    @Published var liveStatus = "Stale"
    @Published var sharedSpaces: [SharedSpace] = []
    @Published var noSpacesShared = false
    @Published var hiddenSpaceIds = Set(UserDefaults.standard.stringArray(forKey: "hiddenSharedSpaces") ?? []) {
        didSet { UserDefaults.standard.set(Array(hiddenSpaceIds), forKey: "hiddenSharedSpaces") }
    }
    var visibleRequests: [ActionRequest] { requests.filter { canView(spaceId: $0.spaceId) } }
    var visibleChats: [Chat] { chats.filter { canView(spaceId: $0.spaceId) } }
    func canView(spaceId: String?) -> Bool {
        if demo { return true }
        guard !connection.hasPrefix("Disconnected"), let spaceId else { return false }
        return !hiddenSpaceIds.contains(spaceId) && sharedSpaces.contains(where: { $0.id == spaceId })
    }
    func refreshAppearance() async { if !demo, let appearance: MacAppearance = try? await request("/appearance") { macAppearance = appearance } }
    func refreshSharedSpaces() async {
        if demo { return }
        do {
            let latest: SharedSpaceList = try await request("/spaces")
            let changed = Set(latest.spaces.map(\.id)) != Set(sharedSpaces.map(\.id))
            sharedSpaces = latest.spaces; noSpacesShared = latest.noSpacesShared
            if changed || connection.hasPrefix("Disconnected") { requests = []; chats = []; await refresh() }
        } catch { connection = "Disconnected · showing stale data" }
    }
    func watchEvents() async {
        guard !demo else { liveStatus = "Demo"; return }
        var delay: UInt64 = 1
        while !Task.isCancelled && !demo {
            do {
                let base = try baseURL()
                let path = activeChatId.map { "/events?chat=\($0)" } ?? "/events"
                guard let url = URL(string: path, relativeTo: base)?.absoluteURL, let token = try token(for: base) else { throw BridgeError.credential }
                var req = URLRequest(url: url); req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization"); req.timeoutInterval = 35
                let session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil)
                defer { session.invalidateAndCancel() }
                let (bytes, response) = try await session.bytes(for: req)
                guard let http = response as? HTTPURLResponse, http.statusCode == 200 else { throw BridgeError.response }
                liveStatus = "Live"; delay = 1; await refresh(); await refreshAppearance()
                for try await line in bytes.lines {
                    if Task.isCancelled { return }
                    if line == "event: appearance" { await refreshAppearance() }
                    if line == "event: refresh" { await refresh() }
                    if line == "event: transcript" { transcriptRevision += 1 }
                }
                throw BridgeError.response
            } catch {
                if Task.isCancelled { return }
                liveStatus = "Stale · reconnecting"; await refresh()
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                delay = min(delay * 2, 30)
            }
        }
    }
    private func baseURL() throws -> URL {
        guard let components = URLComponents(string: address), let host = components.host, components.user == nil, components.password == nil, components.query == nil, components.fragment == nil, (components.path.isEmpty || components.path == "/") else { throw BridgeError.address }
        let local = host == "127.0.0.1" || host == "localhost"
        guard components.scheme == "https" || (components.scheme == "http" && local) else { throw BridgeError.address }
        guard let url = components.url else { throw BridgeError.address }; return url
    }
    private func token(for base: URL) throws -> String? {
        guard let stored = credential.read() else { return nil }
        guard stored.origin == canonicalOrigin(base) else { throw BridgeError.credential }
        return stored.token
    }
    private func canonicalOrigin(_ url: URL) -> String {
        var c = URLComponents(); c.scheme = url.scheme?.lowercased(); c.host = url.host?.lowercased()
        if let port = url.port, !((c.scheme == "https" && port == 443) || (c.scheme == "http" && port == 80)) { c.port = port }
        return c.string ?? ""
    }
    private func request<T: Decodable>(_ path: String, body: [String: String]? = nil, authenticated: Bool = true, idempotencyKey: String? = nil) async throws -> T {
        let base = try baseURL()
        guard let url = URL(string: path, relativeTo: base)?.absoluteURL else { throw BridgeError.address }
        var request = URLRequest(url: url); request.timeoutInterval = 20
        if let body { request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type"); request.httpBody = try JSONEncoder().encode(body) }
        if let idempotencyKey { request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key") }
        if authenticated { guard let token = try token(for: base) else { throw BridgeError.credential }; request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        let session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw BridgeError.response }
        if http.statusCode == 401 { credential.clear(); isPaired = false; connection = "Disconnected · pair again"; throw BridgeError.unauthorized }
        guard (200..<300).contains(http.statusCode) else { throw BridgeError.response }
        return try JSONDecoder().decode(T.self, from: data)
    }
    func pair() async {
        do {
            let origin = canonicalOrigin(try baseURL())
            let reply: PairReply = try await request("/pair", body: ["code": pairingCode, "label": UIDevice.current.name], authenticated: false)
            guard canonicalOrigin(try baseURL()) == origin else { throw BridgeError.address }
            try credential.save(BoundCredential(origin: origin, token: reply.token)); isPaired = true; pairingCode = ""; demo = false; hasLoaded = false; requests = []; chats = []; await refresh()
        } catch { connection = "Disconnected · \(error.localizedDescription)" }
    }
    func refresh() async {
        if demo { requests = Demo.requests; chats = Demo.chats; sharedSpaces = []; noSpacesShared = false; connection = "Demo mode"; hasLoaded = true; return }
        isLoading = true
        defer { isLoading = false }
        do {
            async let newSpaces: SharedSpaceList = request("/spaces")
            async let newRequests: SpaceItems<ActionRequest> = request("/requests")
            async let newChats: SpaceItems<Chat> = request("/chats")
            let (spaces, requestsReply, chatsReply) = try await (newSpaces, newRequests, newChats)
            sharedSpaces = spaces.spaces; noSpacesShared = spaces.noSpacesShared || requestsReply.noSpacesShared || chatsReply.noSpacesShared
            requests = noSpacesShared ? [] : requestsReply.items; chats = noSpacesShared ? [] : chatsReply.items
            connection = "Connected · updated now"
            await refreshAppearance()
            hasLoaded = true; lastUpdated = Date()
        } catch BridgeError.unauthorized { connection = "Disconnected · pair again" }
        catch { connection = "Disconnected · showing stale data" }
    }
    func chatLink(_ id: String) async throws -> Chat {
        if demo { guard let chat = chats.first(where: { $0.id == id }) else { throw BridgeError.response }; return chat }
        let chat: Chat = try await request("/chats/\(id)")
        guard canView(spaceId: chat.spaceId) else { throw BridgeError.response }; return chat
    }
    func artifact(_ id: String) async throws -> (ArtifactMetadata, Data) {
        guard id.range(of: "^[A-Za-z0-9_-]{1,128}$", options: .regularExpression) != nil else { throw BridgeError.response }
        if demo { return (ArtifactMetadata(id: id, title: "Sample plan", kind: "markdown", spaceId: "demo", revision: 1), Data("# Sample plan\n\nReview the **design** before proceeding.".utf8)) }
        let metadata: ArtifactMetadata = try await request("/artifacts/\(id)")
        let base = try baseURL()
        guard canView(spaceId: metadata.spaceId), let token = try token(for: base), let url = URL(string: "/artifacts/\(id)/content", relativeTo: base)?.absoluteURL else { throw BridgeError.response }
        var req = URLRequest(url: url); req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization"); req.timeoutInterval = 20
        let session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil); defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200, data.count <= 4 * 1024 * 1024, canView(spaceId: metadata.spaceId) else { throw BridgeError.response }
        return (metadata, data)
    }
    func transcript(_ chat: Chat, offset: Int = 0) async throws -> TranscriptPage {
        if demo { return TranscriptPage(messages: offset == 0 ? Demo.messages : [], olderRemaining: 0, nextOffset: Demo.messages.count) }
        return try await request("/chats/\(chat.id)/transcript?offset=\(offset)")
    }
    private func authenticateWrite() async throws {
        let result = await withCheckedContinuation { continuation in actionAuthenticator.authenticate { continuation.resume(returning: $0) } }
        guard result == .success, actionAllowed() else { throw BridgeError.credential }
    }
    func send(_ chat: Chat, text: String) async throws -> String {
        if demo { return "Demo only · message was not sent" }
        try await authenticateWrite()
        let result: SendReply = try await request("/chats/\(chat.id)/messages", body: ["text": text])
        if result.delivery == "simulated" { return "Fixture only · message was not sent" }
        return result.delivery == "queued" ? "Queued" : "Accepted"
    }
    func stop(_ chat: Chat) async throws {
        if demo { return }
        try await authenticateWrite()
        let _: StopReply = try await request("/chats/\(chat.id)/stop", body: [:])
    }
    func pendingDecision(_ requestId: String) throws -> PendingDecision? { demo ? nil : try decisions.read(requestId) }
    func answer(_ requestId: String, outcome: String, text: String? = nil, optionId: String? = nil) async throws -> PendingDecision {
        let existing = try decisions.read(requestId)
        var decision = existing ?? PendingDecision(key: UUID().uuidString, outcome: outcome, text: text, optionId: optionId)
        guard decision.outcome == outcome && decision.text == text && decision.optionId == optionId else { throw BridgeError.response }
        if demo { guard existing == nil else { throw BridgeError.response }; decision.phase = "resolved"; return decision }
        try await authenticateWrite()
        try decisions.save(requestId, decision: decision)
        var body = ["outcome": decision.outcome]
        if let text = decision.text { body["text"] = text }; if let optionId = decision.optionId { body["optionId"] = optionId }
        let resolveOnly = decision.phase == "sent" || decision.phase == "resolving"
        let result: AnswerResult = try await request("/requests/\(requestId)/\(resolveOnly ? "resolve" : "answer")", body: resolveOnly ? [:] : body, idempotencyKey: decision.key)
        decision.phase = result.state; decision.resolveOnly = result.resolveOnly; try decisions.save(requestId, decision: decision)
        return decision
    }
    func forget() { credential.clear(); isPaired = false; demo = true; requests = Demo.requests; chats = Demo.chats; connection = "Demo mode" }
}

