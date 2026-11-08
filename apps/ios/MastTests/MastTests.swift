import XCTest
@testable import Mast

private final class MemoryCredential: CredentialStore {
    var token: String?
    func read() -> String? { token }
    func save(_ token: String) throws { self.token = token }
    func clear() { token = nil }
}
private final class StubProtocol: URLProtocol {
    static var reply: (URLRequest) throws -> (Int, String) = { _ in (500, "{}") }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            let (status, body) = try Self.reply(request)
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}
@MainActor
final class MastTests: XCTestCase {
    private func model(_ credential: MemoryCredential) -> Companion {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubProtocol.self]
        let model = Companion(credential: credential, configuration: configuration)
        model.demo = false
        return model
    }
    func testDemoFixturesAndDecoding() throws {
        XCTAssertFalse(Demo.requests.isEmpty)
        XCTAssertFalse(Demo.chats.isEmpty)
        XCTAssertFalse(Demo.messages.isEmpty)
        let encoded = try JSONEncoder().encode(Demo.requests)
        XCTAssertEqual(try JSONDecoder().decode([ActionRequest].self, from: encoded).first?.text, Demo.requests.first?.text)
        let chats = try JSONEncoder().encode(Demo.chats)
        XCTAssertEqual(try JSONDecoder().decode([Chat].self, from: chats).count, Demo.chats.count)
        let request = try JSONDecoder().decode(ActionRequest.self, from: Data("""
        {"id":"r","space":"Mast","kind":"review","text":"Review","detail":"Details","options":["Yes"],"facts":["Status: Ready"],"links":["Plan"],"origin":"Chat"}
        """.utf8))
        XCTAssertEqual(request.options, ["Yes"])
        XCTAssertEqual(request.origin, "Chat")
    }
    func testRefreshLoadsInboxAndChatsAndRetainsStaleData() async throws {
        let credential = MemoryCredential(); try credential.save("token")
        let model = model(credential)
        StubProtocol.reply = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer token")
            if request.url!.path == "/requests" { return (200, "[{\"id\":\"r\",\"space\":\"Space\",\"kind\":\"review\",\"text\":\"Review\",\"detail\":\"\",\"options\":[],\"facts\":[],\"links\":[],\"origin\":\"chat\"}]") }
            return (200, "[{\"id\":\"c\",\"title\":\"Chat\",\"space\":\"Space\",\"status\":\"working\"}]")
        }
        await model.refresh()
        XCTAssertEqual(model.requests.first?.text, "Review")
        XCTAssertEqual(model.chats.first?.title, "Chat")
        XCTAssertTrue(model.connection.hasPrefix("Connected"))
        StubProtocol.reply = { _ in throw URLError(.notConnectedToInternet) }
        await model.refresh()
        XCTAssertEqual(model.requests.first?.text, "Review")
        XCTAssertTrue(model.connection.contains("stale"))
    }
    func testPairAndSendAcceptedAndError() async throws {
        let credential = MemoryCredential(); let model = model(credential)
        StubProtocol.reply = { request in
            switch request.url!.path {
            case "/pair": return (200, "{\"token\":\"device\"}")
            case "/requests", "/chats": return (200, "[]")
            default: return (200, "{\"delivery\":\"accepted\"}")
            }
        }
        model.pairingCode = "code"; await model.pair()
        XCTAssertEqual(credential.read(), "device")
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "working")
        let sent = try await model.send(chat, text: "hello")
        XCTAssertEqual(sent, "Accepted")
        StubProtocol.reply = { _ in (503, "{}") }
        do { _ = try await model.send(chat, text: "hello"); XCTFail("Expected error") } catch { }
        model.forget(); XCTAssertNil(credential.read())
    }
    func testUnauthorizedClearsCredentialAndRequiresPairing() async throws {
        let credential = MemoryCredential(); try credential.save("revoked")
        let model = model(credential)
        StubProtocol.reply = { _ in (401, "{}") }
        await model.refresh()
        XCTAssertNil(credential.read())
        XCTAssertTrue(model.connection.contains("pair again"))
    }
    func testTranscriptPagesAndChatVisibility() async throws {
        let credential = MemoryCredential(); try credential.save("token")
        let model = model(credential)
        StubProtocol.reply = { request in
            XCTAssertEqual(request.url?.query, "offset=40")
            return (200, "{\"messages\":[{\"id\":\"m\",\"role\":\"user\",\"text\":\"Worker updates: done\",\"author\":\"system\"}],\"olderRemaining\":0,\"nextOffset\":41}")
        }
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "working")
        let page = try await model.transcript(chat, offset: 40)
        XCTAssertEqual(page.messages.first?.author, "system")
        XCTAssertEqual(page.nextOffset, 41)
        XCTAssertFalse(Chat(id: "w", title: "Worker", space: "", status: "idle", isWorker: true).visible(showWorkers: false))
        XCTAssertTrue(Chat(id: "w", title: "Worker", space: "", status: "idle", isWorker: true).visible(showWorkers: true))
        XCTAssertFalse(Chat(id: "n", title: "No user", space: "", status: "idle", hasUserMessage: false).visible(showWorkers: true))
        XCTAssertTrue(Chat(id: "r", title: "Request", space: "", status: "idle", isWorker: true, hasOpenRequest: true).visible(showWorkers: false))
    }
}

import SwiftUI

@MainActor
final class HorizonSnapshotTests: XCTestCase {
    func testRenderScreens() throws {
        let model = Companion(credential: MemoryCredential())
        let offline = Companion(credential: MemoryCredential())
        offline.connection = "Disconnected · showing stale data"
        let longMessages = (1...40).map { TranscriptMessage(id: "long-\($0)", role: "assistant", text: "Message \($0): The latest update is ready for review.", author: "agent") }
        let screens: [(String, AnyView)] = [
            ("inbox", AnyView(InboxScreen(model: model))),
            ("request-detail", AnyView(RequestDetail(request: Demo.requests[0]))),
            ("chats", AnyView(ChatsScreen(model: model))),
            ("chat-detail", AnyView(ChatDetail(chat: Demo.chats[0], model: model, initialMessages: Demo.messages))),
            ("long-chat-bottom", AnyView(ChatDetail(chat: Demo.chats[0], model: model, initialMessages: Array(longMessages.suffix(3))))),
            ("connect", AnyView(ConnectScreen(model: model))),
            ("offline", AnyView(VStack(spacing: 0) { OfflineBanner(text: offline.connection); InboxScreen(model: offline) }))
        ]
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/snapshots")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for scheme in [ColorScheme.light, .dark] {
            for (name, screen) in screens {
                let renderer = ImageRenderer(content: screen.environment(\.colorScheme, scheme).environment(\.snapshotRendering, true).frame(width: 393, height: 852))
                renderer.scale = 1
                let png = try XCTUnwrap(try XCTUnwrap(renderer.uiImage).pngData())
                try png.write(to: directory.appendingPathComponent("\(name)-\(scheme == .dark ? "dark" : "light").png"))
            }
        }
    }
}
