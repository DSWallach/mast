import XCTest
@testable import Mast

private final class MemoryCredential: CredentialStore {
    var token: BoundCredential?
    func read() -> BoundCredential? { token }
    func save(_ token: BoundCredential) throws { self.token = token }
    func clear() { token = nil }
}
private final class MemoryDecisionStore: DecisionStore {
    var values: [String: PendingDecision] = [:]
    func read(_ requestId: String) throws -> PendingDecision? { values[requestId] }
    func save(_ requestId: String, decision: PendingDecision) throws { values[requestId] = decision }
}
private struct RefusingAuthenticator: DeviceAuthenticator { func authenticate(completion: @escaping (AuthenticationResult) -> Void) { completion(.failure) } }
private struct ImmediateAuthenticator: DeviceAuthenticator { func authenticate(completion: @escaping (AuthenticationResult) -> Void) { completion(.success) } }
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
        let model = Companion(credential: credential, configuration: configuration, actionAllowed: { true }, actionActive: { true })
        model.demo = false
        return model
    }
    private func enableWrites(_ model: Companion, requestIDs: [String] = []) {
        model.sharedSpaces = [SharedSpace(id: "s", name: "Space", allow: "send")]; model.noSpacesShared = false; model.connection = "Connected"
        model.requests = requestIDs.map { ActionRequest(id: $0, space: "Space", kind: "ask", text: "Question", detail: "", options: [], facts: [], links: [], origin: "Chat", spaceId: "s") }
    }
    func testPairingErrorsAndDeliveryClassification() {
        let reasons = ["wrong-code", "expired", "used", "attempts-exhausted"]
        for reason in reasons {
            let data = Data("{\"reason\":\"\(reason)\"}".utf8)
            XCTAssertFalse(pairingHTTPMessage(status: 401, data: data).isEmpty)
        }
        for error in ["identity forbidden", "host forbidden", "funnel forbidden", "pairing closed"] {
            let data = Data("{\"error\":\"\(error)\"}".utf8)
            XCTAssertFalse(pairingHTTPMessage(status: 403, data: data).isEmpty)
        }
        XCTAssertTrue(pairingHTTPMessage(status: 400, data: Data("{\"error\":\"device label required\"}".utf8)).contains("device label required"))
        XCTAssertTrue(pairingNetworkMessage(URLError(.serverCertificateUntrusted)).contains("TLS"))
        XCTAssertTrue(pairingNetworkMessage(URLError(.notConnectedToInternet)).contains("Network"))
        XCTAssertTrue(classifyWriteNetworkError(URLError(.cannotConnectToHost)).localizedDescription.contains("Not sent"))
        XCTAssertTrue(classifyWriteNetworkError(URLError(.timedOut)).localizedDescription.contains("Delivery unknown"))
    }
    func testPairingCannotRunTwiceAndShowsErrors() async throws {
        let credential = MemoryCredential(); let model = model(credential); model.pairingCode = "code"
        let lock = NSLock(); var attempts = 0
        StubProtocol.reply = { _ in lock.lock(); attempts += 1; lock.unlock(); Thread.sleep(forTimeInterval: 0.15); return (401, "{\"reason\":\"expired\"}") }
        async let first: Void = model.pair()
        async let second: Void = model.pair()
        _ = await (first, second)
        XCTAssertEqual(attempts, 1); XCTAssertFalse(model.pairingInProgress)
        XCTAssertTrue(model.pairingError?.contains("expired") == true)
    }
    func testCapabilitiesBlockWritesBeforeAuthentication() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        let auth = MockAuthenticator()
        let model = Companion(credential: credential, decisions: MemoryDecisionStore(), configuration: configuration, actionAllowed: { true }, actionActive: { true }); model.demo = false; enableWrites(model, requestIDs: ["r"])
        model.sharedSpaces = [SharedSpace(id: "s", name: "Space", allow: "view")]
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "working", spaceId: "s")
        do { _ = try await model.send(chat, text: "hello"); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("Not allowed")) }
        do { try await model.stop(chat); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("Not allowed")) }
        do { _ = try await model.answer("r", outcome: "answered", text: "yes"); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("Not allowed")) }
        XCTAssertNil(auth.completion)
        model.sharedSpaces = [SharedSpace(id: "s", name: "Space", allow: "answer")]
        XCTAssertTrue(model.canPerform(spaceId: "s", level: "answer")); XCTAssertFalse(model.canPerform(spaceId: "s", level: "send"))
        XCTAssertFalse(model.canPerform(spaceId: "missing", level: "answer"))
    }
    func testWriteErrorsAreReportedPrecisely() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let model = model(credential); enableWrites(model)
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "working", spaceId: "s")
        for (status, body, expected) in [(403, "{\"error\":\"capability\",\"need\":\"send\"}", "Not allowed"), (429, "{\"retryAfter\":12}", "12 seconds"), (503, "{}", "Delivery unknown")] {
            StubProtocol.reply = { _ in (status, body) }
            do { _ = try await model.send(chat, text: "hello"); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains(expected), error.localizedDescription) }
        }
        StubProtocol.reply = { _ in throw URLError(.notConnectedToInternet) }
        do { _ = try await model.send(chat, text: "hello"); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("Not sent")) }
        StubProtocol.reply = { _ in throw URLError(.timedOut) }
        do { _ = try await model.send(chat, text: "hello"); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("Delivery unknown")) }
        StubProtocol.reply = { _ in (401, "{}") }
        do { _ = try await model.send(chat, text: "hello"); XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("Pair again")) }
        XCTAssertNil(credential.read())
    }
    func testLoadStateUsesFailureGraceAndRetainsContent() {
        var state = LoadStateMachine(); var now = Date(timeIntervalSince1970: 100)
        XCTAssertEqual(state.state, .idle); state.begin(); XCTAssertEqual(state.state, .loading)
        now.addTimeInterval(10); state.advance(to: now); XCTAssertEqual(state.state, .loading)
        state.fail(at: now); now.addTimeInterval(2); state.advance(to: now); XCTAssertEqual(state.state, .loading)
        state.succeed(empty: false); now.addTimeInterval(5); state.advance(to: now); XCTAssertEqual(state.state, .loaded)
        state.begin(); state.fail(at: now); now.addTimeInterval(3); state.advance(to: now); XCTAssertEqual(state.state, .stale)
        state.begin(); XCTAssertEqual(state.state, .stale); state.succeed(empty: true); XCTAssertEqual(state.state, .empty)
        state.reset(); state.begin(); state.fail(at: now); now.addTimeInterval(3); state.advance(to: now); XCTAssertEqual(state.state, .offline)
    }
    func testArtifactKindsAndNavigationAreRestricted() {
        for kind in ["htmlPage", "htmlFragment", "html-page", "html-fragment"] { XCTAssertEqual(artifactDisplayKind(kind), "html") }
        XCTAssertEqual(artifactDisplayKind("image"), "image"); XCTAssertEqual(artifactDisplayKind("mermaid"), "other")
        XCTAssertTrue(allowedArtifactNavigation(URL(string: "about:blank")!))
        XCTAssertFalse(allowedArtifactNavigation(URL(string: "data:text/html,hello")!))
        XCTAssertFalse(allowedArtifactNavigation(URL(string: "https://example.com")!))
    }
    func testArtifactLinkRouting() {
        if case .artifact(let id) = companionLink(URL(string: "mast-artifact:abc123")!) { XCTAssertEqual(id, "abc123") } else { XCTFail("artifact route") }
        if case .chat(let id) = companionLink(URL(string: "mast-chat:chat1")!) { XCTAssertEqual(id, "chat1") } else { XCTFail("chat route") }
        if case .safari = companionLink(URL(string: "https://example.com")!) {} else { XCTFail("external route") }
        XCTAssertNil(companionLink(URL(string: "http://example.com")!))
        if case .mac = companionLink(URL(string: "file:///secret")!) {} else { XCTFail("file route") }
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
    func testWritesRequireAuthenticatedUnlockAndRejectBackground() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        let authenticator = MockAuthenticator(), lock = AppLock(authenticator: authenticator)
        var prompts = 0, requests = 0
        let model = Companion(credential: credential, decisions: MemoryDecisionStore(), configuration: configuration, actionAllowed: { lock.allowsWrite }, actionGeneration: { lock.authorizationGeneration }, actionActive: { true }); model.demo = false; enableWrites(model, requestIDs: ["r"])
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "working", spaceId: "s")
        StubProtocol.reply = { request in requests += 1; return request.url!.path.hasSuffix("stop") ? (200, "{}") : (200, "{\"delivery\":\"accepted\"}") }
        do { _ = try await model.send(chat, text: "hello"); XCTFail() } catch {}
        lock.foreground(timeout: 60); prompts += 1; authenticator.completion?(.noPasscode); await Task.yield(); lock.continueWithoutPasscode()
        XCTAssertFalse(lock.allowsWrite)
        do { try await model.stop(chat); XCTFail() } catch {}
        lock.background(); lock.foreground(timeout: 0); prompts += 1; authenticator.completion?(.success); await Task.yield()
        XCTAssertTrue(lock.allowsWrite)
        _ = try await model.send(chat, text: "hello"); XCTAssertEqual(requests, 1); XCTAssertEqual(prompts, 2)
        lock.background()
        do { _ = try await model.answer("r", outcome: "answered", text: "yes"); XCTFail() } catch {}
        XCTAssertEqual(requests, 1)
    }
    func testWriteCancelledWhenGenerationChangesBeforeDispatch() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        var checks = 0, sent = false
        let model = Companion(credential: credential, configuration: configuration, actionAllowed: { true }, actionGeneration: { checks += 1; return checks < 3 ? 0 : 1 }, actionActive: { true }); model.demo = false; enableWrites(model)
        StubProtocol.reply = { _ in sent = true; return (200, "{}") }
        do { _ = try await model.send(Chat(id: "c", title: "Chat", space: "Space", status: "idle", spaceId: "s"), text: "hello"); XCTFail() } catch { XCTAssertEqual(error.localizedDescription, "Cancelled") }
        XCTAssertFalse(sent)
    }
    func testFirstLaunchClearsSurvivingKeychainOnlyOnce() throws {
        let suite = "mast-first-launch-\(UUID().uuidString)"; let defaults = try XCTUnwrap(UserDefaults(suiteName: suite)); defer { defaults.removePersistentDomain(forName: suite) }
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "https://example.com", token: "old")); var clears = 0
        clearKeychainOnFirstLaunch(defaults: defaults, credential: credential) { clears += 1 }; XCTAssertNil(credential.read()); XCTAssertEqual(clears, 1)
        try credential.save(BoundCredential(origin: "https://example.com", token: "new")); clearKeychainOnFirstLaunch(defaults: defaults, credential: credential) { clears += 1 }; XCTAssertEqual(credential.read()?.token, "new"); XCTAssertEqual(clears, 1)
    }
    func testCredentialIsNeverSentToAnotherOrigin() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "https://good.ts.net", token: "secret"))
        let model = model(credential); XCTAssertEqual(model.address, "https://good.ts.net")
        model.address = "https://attacker.example"; var sent = false
        StubProtocol.reply = { _ in sent = true; return (200, "{}") }
        await model.refresh(); XCTAssertFalse(sent); XCTAssertNotNil(credential.read())
    }
    func testRefreshLoadsInboxAndChatsAndRetainsStaleData() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
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
        XCTAssertEqual(model.loadState, .loaded)
        model.hiddenSpaceIds.removeAll(); XCTAssertEqual(model.visibleRequests.count, 1)
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
        XCTAssertEqual(credential.read()?.token, "device")
        XCTAssertTrue(model.noSpacesShared)
        XCTAssertTrue(model.visibleRequests.isEmpty)
        enableWrites(model)
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "working", spaceId: "s")
        let sent = try await model.send(chat, text: "hello")
        XCTAssertEqual(sent, "Accepted")
        StubProtocol.reply = { _ in (503, "{}") }
        do { _ = try await model.send(chat, text: "hello"); XCTFail("Expected error") } catch { }
        model.forget(); XCTAssertNil(credential.read())
    }
    func testUnauthorizedClearsCredentialAndRequiresPairing() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "revoked"))
        let model = model(credential)
        StubProtocol.reply = { _ in (401, "{}") }
        await model.refresh()
        XCTAssertNil(credential.read())
        XCTAssertTrue(model.connection.contains("pair again"))
    }
    func testTranscriptPagesAndChatVisibility() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
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
    func testTranscriptCacheIsClearedOnRevocationAndUnshare() async throws {
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let model = model(credential); enableWrites(model)
        let chat = Chat(id: "c", title: "Chat", space: "Space", status: "idle", spaceId: "s")
        let page = TranscriptPage(messages: [TranscriptMessage(id: "m", role: "user", text: "hello")], olderRemaining: 0, nextOffset: 1)
        model.cacheTranscript(chat, page: page); XCTAssertEqual(model.cachedTranscript(chat)?.messages.first?.text, "hello")
        StubProtocol.reply = { request in
            if request.url?.path == "/spaces" { return (200, "{\"spaces\":[],\"noSpacesShared\":true}") }
            return (200, "{\"items\":[],\"noSpacesShared\":true}")
        }
        await model.refresh(); XCTAssertNil(model.cachedTranscript(chat))
        model.cacheTranscript(chat, page: page); model.forget(); XCTAssertNil(model.cachedTranscript(chat))
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
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let store = MemoryDecisionStore()
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        let model = Companion(credential: credential, decisions: store, configuration: configuration, actionAllowed: { true }, actionActive: { true }); model.demo = false; enableWrites(model, requestIDs: ["r"])
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
        let credential = MemoryCredential(); try credential.save(BoundCredential(origin: "http://127.0.0.1:27655", token: "token"))
        let store = MemoryDecisionStore()
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [StubProtocol.self]
        let model = Companion(credential: credential, decisions: store, configuration: configuration, actionAllowed: { true }, actionActive: { true }); model.demo = false; enableWrites(model, requestIDs: ["ask", "choose", "approve", "decline"])
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
    var body: some View { MarkdownReply(text: "# Release notes\n\nA **bold** and *italic* reply with `code` and a [link](https://example.com).\n\n- First item\n- Second item\n1. Numbered item\n\n```swift\nlet value = 1\n``` ", linkColor: MastStyle(id: .horizon).provider(Demo.chats[0], scheme)).padding(22).foregroundStyle(MastStyle(id: .horizon).provider(Demo.chats[0], scheme)) }
}

@MainActor
final class HorizonSnapshotTests: XCTestCase {
    func testRenderStyleMatrix() throws {
        let model = Companion(credential: MemoryCredential())
        let fixtureImage = UIGraphicsImageRenderer(size: CGSize(width: 120, height: 80)).pngData { context in UIColor.systemGreen.setFill(); context.fill(CGRect(x: 0, y: 0, width: 120, height: 80)) }
        func artifact(_ kind: String, _ data: Data) -> AnyView { AnyView(ArtifactScreen(id: "fixture", model: model, initial: (ArtifactMetadata(id: "fixture", title: "Sample artifact", kind: kind, spaceId: "demo", revision: 1), data))) }
        let screens: [(String, AnyView)] = [
            ("artifact-markdown", artifact("markdown", Data("# Sample plan\n\nPlease review **the design**.".utf8))),
            ("artifact-html", artifact("htmlFragment", Data("<h1>Mockup</h1>".utf8))),
            ("artifact-image", artifact("image", fixtureImage)),
            ("request-markdown", AnyView(RequestDetail(request: ActionRequest(id: "r", space: "Mast", kind: "ask", text: "Review [plan](mast-artifact:fixture)", detail: "See the **design**: [open](mast-artifact:fixture)", options: [], facts: ["Status: **ready**"], links: [], origin: "Chat"), model: model))),("chat", AnyView(ChatDetail(chat: Demo.chats[0], model: model, initialMessages: Demo.messages))), ("inbox", AnyView(InboxScreen(model: model))), ("chats", AnyView(ChatsScreen(model: model)))]
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("build/style-snapshots")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        for id in MastStyleID.allCases { for scheme in [ColorScheme.light, .dark] { for (name, screen) in screens {
            let style = MastStyle(id: id)
            let renderer = ImageRenderer(content: screen.environment(\.mastStyle, style).environment(\.colorScheme, scheme).environment(\.snapshotRendering, true).frame(width: 393, height: 852, alignment: .topLeading).background(style.surface(scheme)))
            renderer.scale = 1
            try XCTUnwrap(try XCTUnwrap(renderer.uiImage).pngData()).write(to: directory.appendingPathComponent("\(name)-\(id.rawValue)-\(scheme == .dark ? "dark" : "light").png"))
        } } }
    }
    func testRenderScreens() throws {
        let model = Companion(credential: MemoryCredential())
        let offline = Companion(credential: MemoryCredential())
        offline.demo = false; offline.connection = "Disconnected · showing stale data"; offline.loadState = .offline
        let loading = Companion(credential: MemoryCredential()); loading.demo = false; loading.sharedSpaces = [SharedSpace(id: "s", name: "Space", allow: "send")]
        let loadingChat = Chat(id: "loading", title: "Chat", space: "Space", status: "working", spaceId: "s")
        let empty = Companion(credential: MemoryCredential()); empty.requests = []
        let iconPath = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("Mast/Assets.xcassets/AppIcon.appiconset/AppIcon.png")
        let icon = try XCTUnwrap(UIImage(contentsOfFile: iconPath.path))
        let longMessages = (1...40).map { TranscriptMessage(id: "long-\($0)", role: "assistant", text: "Message \($0): The latest update is ready for review.", author: "agent") }
        let screens: [(String, AnyView)] = [
            ("loading-inbox", AnyView(InboxScreen(model: loading))),
            ("loading-chats", AnyView(ChatsScreen(model: loading))),
            ("loading-chat-detail", AnyView(ChatDetail(chat: loadingChat, model: loading))),
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
                let renderer = ImageRenderer(content: screen.environment(\.colorScheme, scheme).environment(\.snapshotRendering, true).frame(width: 393, height: 852, alignment: .topLeading).background(MastStyle(id: .horizon).surface(scheme)))
                renderer.scale = 1
                let png = try XCTUnwrap(try XCTUnwrap(renderer.uiImage).pngData())
                try png.write(to: directory.appendingPathComponent("\(name)-\(scheme == .dark ? "dark" : "light").png"))
            }
        }
    }
}
