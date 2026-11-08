import SwiftUI
import Security

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

