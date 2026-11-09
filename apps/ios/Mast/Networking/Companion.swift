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
    static func clearAll() { SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: "computer.mikey.mast.companion.decisions"] as CFDictionary) }
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
func clearKeychainOnFirstLaunch(defaults: UserDefaults, credential: any CredentialStore, clearDecisions: () -> Void) {
    guard !defaults.bool(forKey: "companionFirstLaunchComplete") else { return }
    credential.clear(); clearDecisions(); defaults.set(true, forKey: "companionFirstLaunchComplete")
}
enum BridgeError: LocalizedError {
    case address, credential, response, unauthorized, capability, notSent, deliveryUnknown, rateLimited(Int), pairing(String)
    var errorDescription: String? {
        switch self {
        case .address: return "Check the bridge URL. Use HTTPS, or HTTP on simulator loopback."
        case .credential: return "Authentication or device credential unavailable."
        case .response: return "Bridge request failed."
        case .unauthorized: return "Device credential revoked. Pair again."
        case .capability: return "Not allowed for this Space."
        case .notSent: return "Not sent. Check the connection."
        case .deliveryUnknown: return "Delivery unknown. Check the chat before retrying."
        case .rateLimited(let seconds): return "Slow down. Try again in \(seconds) seconds."
        case .pairing(let message): return message
        }
    }
}
private struct BridgeFailure: Decodable { var error: String?; var reason: String?; var retryAfter: Int? }
func pairingHTTPMessage(status: Int, data: Data) -> String {
    let failure = try? JSONDecoder().decode(BridgeFailure.self, from: data)
    if status == 401 {
        switch failure?.reason {
        case "wrong-code": return "Wrong pairing code. Check the code and try again."
        case "expired": return "Pairing code expired. Restart the bridge with --pair for a new code."
        case "used": return "This pairing code has already been used. Start a new pairing window."
        case "attempts-exhausted": return "Too many attempts. Start a new pairing window."
        default: return "Wrong or expired pairing code, or the code was already used. Start a new pairing window."
        }
    }
    if status == 403 {
        switch failure?.error { case "host forbidden": return "Bridge Host is not allowed. Check the bridge --url option."
        case "funnel forbidden": return "Funnel access is refused. Use the approved Tailscale Serve path."
        case "pairing closed": return "Pairing is closed. Restart the bridge with --pair."
        case "origin forbidden": return "Bridge rejected the request origin."
        default: return "Tailscale identity did not match. Check the bridge --tailscale-user setting." }
    }
    if status == 400, let error = failure?.error {
        let safe = String(String.UnicodeScalarView(error.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.prefix(200)))
        return safe.isEmpty ? "The bridge rejected the pairing request." : "Bridge rejected pairing: \(safe)"
    }
    if status == 415 { return "The bridge rejected the pairing request format." }
    return "Bridge pairing failed (HTTP \(status)). Check the Mac bridge."
}
func pairingNetworkMessage(_ error: URLError) -> String {
    switch error.code { case .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateHasUnknownRoot, .serverCertificateNotYetValid, .secureConnectionFailed: return "TLS connection failed. Check the HTTPS certificate and Tailscale connection."
    default: return "Network connection failed. Check that the Mac bridge and Tailscale are reachable." }
}
func classifyWriteNetworkError(_ error: URLError) -> BridgeError {
    switch error.code { case .notConnectedToInternet, .cannotFindHost, .dnsLookupFailed, .cannotConnectToHost, .secureConnectionFailed, .serverCertificateUntrusted, .serverCertificateHasBadDate, .serverCertificateHasUnknownRoot, .serverCertificateNotYetValid: return .notSent
    default: return .deliveryUnknown }
}
struct ArtifactMetadata: Decodable { let id: String, title: String, kind: String, spaceId: String; let revision: Int }
private struct PairReply: Decodable { let token: String }
private struct SendReply: Decodable { let delivery: String }
private struct StopReply: Decodable { let status: String }
private final class NoRedirect: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
enum LoadState: Equatable { case idle, loading, loaded, empty, stale, offline }

struct LoadStateMachine {
    private(set) var state: LoadState = .idle
    private(set) var hasContent = false
    private var failureAt: Date?
    mutating func begin() { failureAt = nil; if !hasContent { state = .loading } else if state != .stale && state != .empty { state = .loaded } }
    mutating func succeed(empty: Bool) { failureAt = nil; hasContent = true; state = empty ? .empty : .loaded }
    mutating func fail(at time: Date) { failureAt = time }
    mutating func advance(to time: Date) {
        guard let failureAt, time.timeIntervalSince(failureAt) >= 3 else { return }
        state = hasContent ? .stale : .offline
    }
    mutating func reset() { self = LoadStateMachine() }
}

@MainActor
final class Companion: ObservableObject {
    private let credential: any CredentialStore
    private let decisions: any DecisionStore
    private let configuration: URLSessionConfiguration
    private let actionAuthenticator: any DeviceAuthenticator
    private let actionAllowed: @MainActor () -> Bool
    init(credential: any CredentialStore = Credential(), decisions: any DecisionStore = KeychainDecisionStore(), configuration: URLSessionConfiguration = .ephemeral, actionAuthenticator: any DeviceAuthenticator = LocalDeviceAuthenticator(), actionAllowed: @escaping @MainActor () -> Bool = { UIApplication.shared.applicationState == .active }) { self.actionAllowed = actionAllowed; self.actionAuthenticator = actionAuthenticator; self.credential = credential; self.decisions = decisions; self.configuration = configuration; if credential is Credential { clearKeychainOnFirstLaunch(defaults: .standard, credential: credential, clearDecisions: KeychainDecisionStore.clearAll) }; if let stored = credential.read() { address = stored.origin; isPaired = true } }
    @Published var demo = true {
        didSet { if oldValue != demo { clearCachedContent() } }
    }
    @Published private(set) var isPaired = false
    @Published var address = "http://127.0.0.1:27655"
    @Published var requests = Demo.requests
    @Published var chats = Demo.chats
    @Published var connection = "Demo mode"
    @Published var pairingCode = ""
    @Published private(set) var pairingInProgress = false
    @Published var pairingError: String?
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
    @Published var liveStatus = ""
    @Published var loadState: LoadState = .idle
    private var loadMachine = LoadStateMachine()
    private var failureTask: Task<Void, Never>?
    private var streamFailureTask: Task<Void, Never>?
    private var refreshGeneration = 0
    private var transcriptCache: [String: TranscriptPage] = [:]
    private var transcriptSpaces: [String: String] = [:]
    func cachedTranscript(_ chat: Chat) -> TranscriptPage? { transcriptCache[chat.id] }
    func cacheTranscript(_ chat: Chat, page: TranscriptPage) { transcriptCache[chat.id] = page; transcriptSpaces[chat.id] = chat.spaceId }
    private func clearCachedContent() {
        refreshGeneration += 1; failureTask?.cancel(); streamFailureTask?.cancel()
        transcriptCache.removeAll(); transcriptSpaces.removeAll(); requests = []; chats = []; sharedSpaces = []; noSpacesShared = false
        loadMachine.reset(); loadState = .idle; hasLoaded = false; isLoading = false; liveStatus = ""
    }
    private func reportLoadFailure() {
        loadMachine.fail(at: Date()); failureTask?.cancel()
        failureTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard !Task.isCancelled, let self else { return }
            self.loadMachine.advance(to: Date()); self.loadState = self.loadMachine.state
            self.connection = self.isPaired ? "Disconnected · showing stale data" : "Disconnected · pair again"
        }
    }
    private func reportStreamFailure() {
        guard streamFailureTask == nil else { return }
        streamFailureTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            guard !Task.isCancelled, let self else { return }
            self.liveStatus = "Stale · reconnecting"
        }
    }
    @Published var sharedSpaces: [SharedSpace] = []
    @Published var noSpacesShared = false
    @Published var hiddenSpaceIds = Set(UserDefaults.standard.stringArray(forKey: "hiddenSharedSpaces") ?? []) {
        didSet { UserDefaults.standard.set(Array(hiddenSpaceIds), forKey: "hiddenSharedSpaces") }
    }
    var visibleRequests: [ActionRequest] { requests.filter { canView(spaceId: $0.spaceId) } }
    var visibleChats: [Chat] { chats.filter { canView(spaceId: $0.spaceId) } }
    func canPerform(spaceId: String?, level: String) -> Bool {
        if demo { return true }
        guard canView(spaceId: spaceId), let space = sharedSpaces.first(where: { $0.id == spaceId }) else { return false }
        let ranks = ["view": 0, "answer": 1, "send": 2]
        return (ranks[space.allow ?? "view"] ?? 0) >= (ranks[level] ?? Int.max)
    }
    func canView(spaceId: String?) -> Bool {
        if demo { return true }
        guard let spaceId else { return false }
        return !hiddenSpaceIds.contains(spaceId) && sharedSpaces.contains(where: { $0.id == spaceId })
    }
    func refreshAppearance() async { if !demo, let appearance: MacAppearance = try? await request("/appearance") { macAppearance = appearance } }
    func refreshSharedSpaces() async {
        if demo { return }
        do {
            let latest: SharedSpaceList = try await request("/spaces")
            let changed = Set(latest.spaces.map { "\($0.id):\($0.allow ?? "view")" }) != Set(sharedSpaces.map { "\($0.id):\($0.allow ?? "view")" })
            sharedSpaces = latest.spaces; noSpacesShared = latest.noSpacesShared
            if changed || connection.hasPrefix("Disconnected") { await refresh() }
        } catch { reportLoadFailure() }
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
                streamFailureTask?.cancel(); streamFailureTask = nil; liveStatus = "Live"; delay = 1; await refresh(); await refreshAppearance()
                for try await line in bytes.lines {
                    if Task.isCancelled { return }
                    if line == "event: appearance" { await refreshAppearance() }
                    if line == "event: refresh" { await refresh() }
                    if line == "event: transcript" { transcriptRevision += 1 }
                }
                throw BridgeError.response
            } catch {
                if Task.isCancelled { streamFailureTask?.cancel(); streamFailureTask = nil; return }
                reportStreamFailure(); await refresh()
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
        let data: Data; let response: URLResponse
        do { (data, response) = try await session.data(for: request) }
        catch let error as URLError {
            if !authenticated { throw BridgeError.pairing(pairingNetworkMessage(error)) }
            if body != nil { throw classifyWriteNetworkError(error) }
            throw error
        }
        guard let http = response as? HTTPURLResponse else { throw body == nil ? BridgeError.response : BridgeError.deliveryUnknown }
        if !authenticated && path == "/pair" && !(200..<300).contains(http.statusCode) { throw BridgeError.pairing(pairingHTTPMessage(status: http.statusCode, data: data)) }
        if http.statusCode == 401 { credential.clear(); isPaired = false; clearCachedContent(); loadState = .offline; connection = "Disconnected · pair again"; throw BridgeError.unauthorized }
        if http.statusCode == 403, (try? JSONDecoder().decode(BridgeFailure.self, from: data).error) == "capability" { throw BridgeError.capability }
        if http.statusCode == 429 { let info = try? JSONDecoder().decode(BridgeFailure.self, from: data); throw BridgeError.rateLimited(max(1, info?.retryAfter ?? Int(http.value(forHTTPHeaderField: "Retry-After") ?? "") ?? 60)) }
        guard (200..<300).contains(http.statusCode) else { throw body != nil && http.statusCode >= 500 ? BridgeError.deliveryUnknown : BridgeError.notSent }
        do { return try JSONDecoder().decode(T.self, from: data) }
        catch { if !authenticated { throw BridgeError.pairing("The bridge sent an invalid pairing response.") }; throw body == nil ? BridgeError.response : BridgeError.deliveryUnknown }
    }
    func pair() async {
        guard !pairingInProgress else { return }
        pairingInProgress = true; pairingError = nil
        defer { pairingInProgress = false }
        do {
            let origin = canonicalOrigin(try baseURL())
            let rawLabel = String(String.UnicodeScalarView(UIDevice.current.name.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.prefix(40))).trimmingCharacters(in: .whitespacesAndNewlines)
            let reply: PairReply = try await request("/pair", body: ["code": pairingCode.trimmingCharacters(in: .whitespacesAndNewlines), "label": rawLabel.isEmpty ? "iPhone" : rawLabel], authenticated: false)
            guard canonicalOrigin(try baseURL()) == origin else { throw BridgeError.address }
            try credential.save(BoundCredential(origin: origin, token: reply.token)); clearCachedContent(); isPaired = true; pairingCode = ""; demo = false; hasLoaded = false; requests = []; chats = []; await refresh()
        } catch { pairingError = error.localizedDescription; connection = "Disconnected" }
    }
    func refresh() async {
        if demo { requests = Demo.requests; chats = Demo.chats; sharedSpaces = []; noSpacesShared = false; connection = "Demo mode"; hasLoaded = true; loadState = .loaded; return }
        refreshGeneration += 1; let generation = refreshGeneration
        failureTask?.cancel(); loadMachine.begin(); loadState = loadMachine.state; isLoading = true
        do {
            async let newSpaces: SharedSpaceList = request("/spaces")
            async let newRequests: SpaceItems<ActionRequest> = request("/requests")
            async let newChats: SpaceItems<Chat> = request("/chats")
            let (spaces, requestsReply, chatsReply) = try await (newSpaces, newRequests, newChats)
            guard generation == refreshGeneration, !demo else { return }
            sharedSpaces = spaces.spaces; noSpacesShared = spaces.noSpacesShared || requestsReply.noSpacesShared || chatsReply.noSpacesShared
            let allowed = Set(spaces.spaces.map(\.id))
            transcriptCache = transcriptCache.filter { transcriptSpaces[$0.key].map(allowed.contains) ?? false }
            transcriptSpaces = transcriptSpaces.filter { allowed.contains($0.value) }
            requests = noSpacesShared ? [] : requestsReply.items; chats = noSpacesShared ? [] : chatsReply.items
            connection = "Connected · updated now"; hasLoaded = true; lastUpdated = Date(); isLoading = false
            loadMachine.succeed(empty: requests.isEmpty && chats.isEmpty); loadState = loadMachine.state
            if activeChatId != nil { transcriptRevision += 1 }
            await refreshAppearance()
        } catch {
            guard generation == refreshGeneration, !demo else { return }
            isLoading = false; reportLoadFailure()
        }
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
        let page: TranscriptPage = try await request("/chats/\(chat.id)/transcript?offset=\(offset)")
        if offset == 0, canView(spaceId: chat.spaceId) { cacheTranscript(chat, page: page) }
        return page
    }
    private func authenticateWrite() async throws {
        let result = await withCheckedContinuation { continuation in actionAuthenticator.authenticate { continuation.resume(returning: $0) } }
        guard result == .success, actionAllowed() else { throw BridgeError.credential }
    }
    func send(_ chat: Chat, text: String) async throws -> String {
        if demo { return "Demo only · message was not sent" }
        guard canPerform(spaceId: chat.spaceId, level: "send") else { throw BridgeError.capability }
        try await authenticateWrite()
        let result: SendReply = try await request("/chats/\(chat.id)/messages", body: ["text": text])
        if result.delivery == "simulated" { return "Fixture only · message was not sent" }
        return result.delivery == "queued" ? "Queued" : "Accepted"
    }
    func stop(_ chat: Chat) async throws {
        if demo { return }
        guard canPerform(spaceId: chat.spaceId, level: "send") else { throw BridgeError.capability }
        try await authenticateWrite()
        let _: StopReply = try await request("/chats/\(chat.id)/stop", body: [:])
    }
    func pendingDecision(_ requestId: String) throws -> PendingDecision? { demo ? nil : try decisions.read(requestId) }
    func answer(_ requestId: String, outcome: String, text: String? = nil, optionId: String? = nil) async throws -> PendingDecision {
        let existing = try decisions.read(requestId)
        var decision = existing ?? PendingDecision(key: UUID().uuidString, outcome: outcome, text: text, optionId: optionId)
        guard decision.outcome == outcome && decision.text == text && decision.optionId == optionId else { throw BridgeError.response }
        if demo { guard existing == nil else { throw BridgeError.response }; decision.phase = "resolved"; return decision }
        guard canPerform(spaceId: requests.first(where: { $0.id == requestId })?.spaceId, level: "answer") else { throw BridgeError.capability }
        try await authenticateWrite()
        try decisions.save(requestId, decision: decision)
        var body = ["outcome": decision.outcome]
        if let text = decision.text { body["text"] = text }; if let optionId = decision.optionId { body["optionId"] = optionId }
        let resolveOnly = decision.phase == "sent" || decision.phase == "resolving"
        let result: AnswerResult = try await request("/requests/\(requestId)/\(resolveOnly ? "resolve" : "answer")", body: resolveOnly ? [:] : body, idempotencyKey: decision.key)
        decision.phase = result.state; decision.resolveOnly = result.resolveOnly; try decisions.save(requestId, decision: decision)
        return decision
    }
    func forgetThisMac() async throws {
        let base = try baseURL()
        guard let token = try token(for: base), let url = URL(string: "/devices/self", relativeTo: base)?.absoluteURL else { throw BridgeError.credential }
        var req = URLRequest(url: url); req.httpMethod = "DELETE"; req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil); defer { session.invalidateAndCancel() }
        let (_, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 || http.statusCode == 401 else { throw BridgeError.response }
        forget()
    }
    func forget() { credential.clear(); isPaired = false; clearCachedContent(); demo = true; requests = Demo.requests; chats = Demo.chats; connection = "Demo mode" }
}

