import XCTest
@testable import Mast

private final class MemoryCredential: CredentialStore {
    var token: String?
    func read() -> String? { token }
    func save(_ token: String) throws { self.token = token }
    func clear() { token = nil }
}
private final class MemoryDecisionStore: DecisionStore {
    var values: [String: PendingDecision] = [:]
    func read(_ requestId: String) throws -> PendingDecision? { values[requestId] }
    func save(_ requestId: String, decision: PendingDecision) throws { values[requestId] = decision }
}
private final class MockAuthenticator: DeviceAuthenticator {
    var completion: ((AuthenticationResult) -> Void)?
    func authenticate(completion: @escaping (AuthenticationResult) -> Void) { self.completion = completion }
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
    func testPairingQRValidation() {
        let pair = parsePairingQR("mast-companion://pair?url=https%3A%2F%2Fexample.ts.net&code=one-time")
        XCTAssertEqual(pair?.url, "https://example.ts.net"); XCTAssertEqual(pair?.code, "one-time")
        XCTAssertNil(parsePairingQR("mast-companion://pair?url=http%3A%2F%2Fevil.example&code=x"))
        XCTAssertNil(parsePairingQR("mast-companion://pair?url=https%3A%2F%2Fexample.ts.net&code=x&code=y"))
    }
    func testBundledDesktopFonts() {
        XCTAssertNotNil(UIFont(name: "EBGaramond-Regular", size: 17.5))
        XCTAssertNotNil(UIFont(name: "Geist-Regular", size: 13))
        XCTAssertNotNil(UIFont(name: "Geist-SemiBold", size: 13))
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
        defer { model.hiddenSpaceIds.removeAll() }
        StubProtocol.reply = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer token")
            if request.url!.path == "/spaces" { return (200, "{\"spaces\":[{\"id\":\"s\",\"name\":\"Space\"}],\"noSpacesShared\":false}") }
            if request.url!.path == "/requests" { return (200, "{\"items\":[{\"id\":\"r\",\"spaceId\":\"s\",\"space\":\"Space\",\"kind\":\"review\",\"text\":\"Review\",\"detail\":\"\",\"options\":[],\"facts\":[],\"links\":[],\"origin\":\"chat\"}],\"noSpacesShared\":false}") }
            return (200, "{\"items\":[{\"id\":\"c\",\"spaceId\":\"s\",\"title\":\"Chat\",\"space\":\"Space\",\"status\":\"working\"}],\"noSpacesShared\":false}")
        }
        await model.refresh()
        XCTAssertEqual(model.requests.first?.text, "Review")
        XCTAssertEqual(model.chats.first?.title, "Chat")
        XCTAssertTrue(model.connection.hasPrefix("Connected"))
        XCTAssertEqual(model.visibleRequests.count, 1)
        model.hiddenSpaceIds.insert("s"); XCTAssertTrue(model.visibleRequests.isEmpty); XCTAssertTrue(model.visibleChats.isEmpty)
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
            case "/spaces": return (200, "{\"spaces\":[],\"noSpacesShared\":true}")
            case "/requests", "/chats": return (200, "{\"items\":[],\"noSpacesShared\":true}")
            default: return (200, "{\"delivery\":\"accepted\"}")
            }
        }
        model.pairingCode = "code"; await model.pair()
        XCTAssertEqual(credential.read(), "device")
        XCTAssertTrue(model.noSpacesShared)
        XCTAssertTrue(model.visibleRequests.isEmpty)
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
    func testSystemMessagePreviewRemovesOnlyHeaders() {
        XCTAssertEqual(systemMessagePreview("[Message from Mast chat Coordinator (abc123)]\nPlease review"), "Please review")
        XCTAssertEqual(systemMessagePreview("[Mast system message, not from the user: auto-continue 1/3]\nContinue work"), "Continue work")
        XCTAssertEqual(systemMessagePreview("Re: \"Approve?\": Yes, proceed"), "Yes, proceed")
        XCTAssertEqual(systemMessagePreview("Worker updates: one finished"), "Worker updates: one finished")
    }
    func testMarkdownBlocks() {
        XCTAssertEqual(parseMarkdownBlocks("# Heading\n\nA **bold** paragraph\n\n- first\n2. second\n\n```swift\nlet x = 1\n```"), [.heading(1, "Heading"), .paragraph("A **bold** paragraph"), .item("•", "first"), .item("2.", "second"), .code("let x = 1")])
    }
    func testMarkdownLinkKeepsDestinationAndProviderStyle() throws {
        let result = markdownInline("[Mast](https://example.com)", linkColor: .green)
        let link = try XCTUnwrap(result.runs.first(where: { $0.link != nil }))
        XCTAssertEqual(link.link?.absoluteString, "https://example.com")
        XCTAssertEqual(link.foregroundColor, .green)
        XCTAssertEqual(link.underlineStyle, .single)
    }
    func testAnswerPersistsIntentAndRetriesOnlyResolution() async throws {
        let credential = MemoryCredential(); try credential.save("token")
        let store = MemoryDecisionStore()
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        let model = Companion(credential: credential, decisions: store, configuration: configuration); model.demo = false
        var key: String?
        StubProtocol.reply = { request in
            key = request.value(forHTTPHeaderField: "Idempotency-Key")
            XCTAssertEqual(request.url?.path, "/requests/r/answer")
            return (200, "{\"state\":\"resolving\",\"resolveOnly\":false}")
        }
        let pending = try await model.answer("r", outcome: "answered", text: "yes")
        XCTAssertEqual(pending.phase, "resolving")
        XCTAssertEqual(try store.read("r")?.key, key)
        StubProtocol.reply = { request in
            XCTAssertEqual(request.url?.path, "/requests/r/resolve")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Idempotency-Key"), key)
            return (200, "{\"state\":\"resolved\",\"resolveOnly\":false}")
        }
        let resolved = try await model.answer("r", outcome: "answered", text: "yes")
        XCTAssertEqual(resolved.phase, "resolved")
    }
    func testAnswerKindsAndDemoCannotOverwriteLiveIntent() async throws {
        let credential = MemoryCredential(); try credential.save("token")
        let store = MemoryDecisionStore()
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        let model = Companion(credential: credential, decisions: store, configuration: configuration); model.demo = false
        var bodies: [[String: String]] = []
        StubProtocol.reply = { request in
            let data: Data
            if let body = request.httpBody { data = body }
            else if let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var result = Data(); var buffer = [UInt8](repeating: 0, count: 1024)
                while stream.hasBytesAvailable { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; result.append(contentsOf: buffer.prefix(count)) }
                data = result
            } else { throw URLError(.cannotParseResponse) }
            bodies.append(try JSONDecoder().decode([String: String].self, from: data))
            return (200, "{\"state\":\"sent\",\"resolveOnly\":false}")
        }
        _ = try await model.answer("ask", outcome: "answered", text: "yes")
        _ = try await model.answer("choose", outcome: "chosen", optionId: "a")
        _ = try await model.answer("approve", outcome: "approved")
        _ = try await model.answer("decline", outcome: "declined")
        XCTAssertEqual(bodies, [["outcome": "answered", "text": "yes"], ["outcome": "chosen", "optionId": "a"], ["outcome": "approved"], ["outcome": "declined"]])
        model.demo = true
        do { _ = try await model.answer("ask", outcome: "answered", text: "yes"); XCTFail("Demo must not overwrite a live decision") } catch { }
        XCTAssertEqual(try store.read("ask")?.phase, "sent")
    }
    func testLockOnColdLaunchBackgroundTimeoutAndStaleAuthentication() async {
        let authenticator = MockAuthenticator(); var now = Date(timeIntervalSince1970: 100)
        let lock = AppLock(authenticator: authenticator, now: { now })
        XCTAssertTrue(lock.locked); XCTAssertTrue(lock.obscured)
        lock.foreground(timeout: 60); XCTAssertTrue(lock.authenticating)
        authenticator.completion?(.success); await Task.yield()
        XCTAssertFalse(lock.locked); XCTAssertFalse(lock.obscured)
        lock.inactive(); XCTAssertTrue(lock.obscured)
        lock.background(); now.addTimeInterval(59); lock.foreground(timeout: 60)
        XCTAssertFalse(lock.locked); XCTAssertFalse(lock.obscured)
        lock.background(); now.addTimeInterval(60); lock.foreground(timeout: 60)
        XCTAssertTrue(lock.locked); XCTAssertTrue(lock.obscured)
        let stale = authenticator.completion; lock.background(); stale?(.success); await Task.yield()
        XCTAssertTrue(lock.locked); XCTAssertTrue(lock.obscured)
    }
    func testAuthenticationFailureAndQuickActionsDefault() async {
        let authenticator = MockAuthenticator(), lock = AppLock(authenticator: MockAuthenticator())
        let failed = AppLock(authenticator: authenticator)
        failed.foreground(timeout: 60); authenticator.completion?(.failure); await Task.yield()
        XCTAssertTrue(failed.locked); XCTAssertTrue(failed.obscured)
        failed.continueWithoutPasscode(); XCTAssertTrue(failed.locked)
        failed.unlock(); authenticator.completion?(.noPasscode); await Task.yield()
        XCTAssertTrue(failed.noPasscode); failed.continueWithoutPasscode(); XCTAssertFalse(failed.locked)
        XCTAssertTrue(lock.locked)
        XCTAssertTrue(requiresConfirmation(quickActions: false))
        XCTAssertFalse(requiresConfirmation(quickActions: true))
    }
}

import SwiftUI

private struct MarkdownPreview: View {
    @Environment(\.colorScheme) private var scheme
    var body: some View { MarkdownReply(text: "# Release notes\n\nA **bold** and *italic* reply with `code` and a [link](https://example.com).\n\n- First item\n- Second item\n1. Numbered item\n\n```swift\nlet value = 1\n``` ", linkColor: Horizon.provider(Demo.chats[0], scheme)).padding(22).foregroundStyle(Horizon.provider(Demo.chats[0], scheme)) }
}

@MainActor
final class HorizonSnapshotTests: XCTestCase {
    func testRenderScreens() throws {
        let model = Companion(credential: MemoryCredential())
        let offline = Companion(credential: MemoryCredential())
        offline.demo = false; offline.connection = "Disconnected · showing stale data"
        let empty = Companion(credential: MemoryCredential()); empty.requests = []
        let iconPath = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("Mast/Assets.xcassets/AppIcon.appiconset/AppIcon.png")
        let icon = try XCTUnwrap(UIImage(contentsOfFile: iconPath.path))
        let longMessages = (1...40).map { TranscriptMessage(id: "long-\($0)", role: "assistant", text: "Message \($0): The latest update is ready for review.", author: "agent") }
        let screens: [(String, AnyView)] = [
            ("inbox", AnyView(InboxScreen(model: model))),
            ("request-detail", AnyView(RequestDetail(request: Demo.requests[0], model: model))),
            ("answer-ask", AnyView(RequestDetail(request: ActionRequest(id: "ask", space: "Mast", kind: "ask", text: "What should we do?", detail: "A decision is needed.", options: [], facts: [], links: [], origin: "Chat"), model: model))),
            ("answer-approve", AnyView(RequestDetail(request: ActionRequest(id: "approve", space: "Mast", kind: "approve", text: "Approve this change?", detail: "Please review.", options: [], facts: [], links: [], origin: "Chat"), model: model))),
            ("answer-choose", AnyView(RequestDetail(request: ActionRequest(id: "choose", space: "Mast", kind: "choose", text: "Pick an option", detail: "Choose one.", options: ["Alpha", "Beta"], facts: [], links: [], origin: "Chat", optionChoices: [AnswerOption(id: "a", label: "Alpha"), AnswerOption(id: "b", label: "Beta")]), model: model))),
            ("chats", AnyView(ChatsScreen(model: model))),
            ("chat-detail", AnyView(ChatDetail(chat: Demo.chats[0], model: model, initialMessages: Demo.messages))),
            ("long-chat-bottom", AnyView(ChatDetail(chat: Demo.chats[0], model: model, initialMessages: Array(longMessages.suffix(3))))),
            ("connect", AnyView(ConnectScreen(model: model))),
            ("markdown-reply", AnyView(MarkdownPreview())),
            ("tool-row", AnyView(ToolActivityRow(text: "exec: cargo test\nexec: git status").padding(22))),
            ("empty-inbox", AnyView(InboxScreen(model: empty))),
            ("offline-full", AnyView(InboxScreen(model: offline))),
            ("inbox-ax3", AnyView(InboxScreen(model: model).dynamicTypeSize(.accessibility3))),
            ("app-icon", AnyView(Image(uiImage: icon).resizable().frame(width: 320, height: 320).clipShape(RoundedRectangle(cornerRadius: 72, style: .continuous)).padding(36))),
            ("locked", AnyView(LockScreen(authenticating: false, unlock: {}))),
            ("offline", AnyView(VStack(spacing: 0) { OfflineBanner(text: offline.connection); InboxScreen(model: offline) }))
        ]
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/snapshots")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for scheme in [ColorScheme.light, .dark] {
            for (name, screen) in screens {
                let renderer = ImageRenderer(content: screen.environment(\.colorScheme, scheme).environment(\.snapshotRendering, true).frame(width: 393, height: 852, alignment: .topLeading).background(Horizon.surface(scheme)))
                renderer.scale = 1
                let png = try XCTUnwrap(try XCTUnwrap(renderer.uiImage).pngData())
                try png.write(to: directory.appendingPathComponent("\(name)-\(scheme == .dark ? "dark" : "light").png"))
            }
        }
    }
}
