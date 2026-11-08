import SwiftUI

struct ActionRequest: Identifiable, Codable {
    let id: String, space: String, kind: String, text: String, detail: String
    let options: [String], facts: [String], links: [String]
    let origin: String
    var priority: String? = nil, item: String? = nil
    var createdAt: Double? = nil
    var optionChoices: [AnswerOption]? = nil
    var spaceId: String? = nil
}
struct AnswerOption: Codable, Identifiable { let id: String, label: String; var detail: String? = nil }
struct PendingDecision: Codable {
    let key: String, outcome: String
    var text: String? = nil, optionId: String? = nil
    var phase: String = "unconfirmed"
    var resolveOnly: Bool? = nil
}
struct AnswerResult: Decodable { let state: String; let resolveOnly: Bool }
struct Chat: Identifiable, Codable {
    let id: String, title: String, space: String, status: String
    var harness: String? = nil, model: String? = nil, lastMessageAt: String? = nil
    var isWorker: Bool? = nil, hasUserMessage: Bool? = nil, hasOpenRequest: Bool? = nil
    var spaceId: String? = nil
    func visible(showWorkers: Bool) -> Bool { hasOpenRequest == true || (isWorker == true ? showWorkers : hasUserMessage != false) }
}
struct TranscriptMessage: Identifiable, Codable {
    let id: String, role: String, text: String
    var author: String? = nil
}
struct TranscriptPage: Decodable { let messages: [TranscriptMessage]; let olderRemaining: Int; let nextOffset: Int }
struct MacAppearance: Decodable { let style: String, layout: String, lightTheme: String, darkTheme: String }
struct SharedSpace: Identifiable, Codable { let id: String, name: String }
struct SharedSpaceList: Decodable { let spaces: [SharedSpace]; let noSpacesShared: Bool }
struct SpaceItems<T: Decodable>: Decodable { let items: [T]; let noSpacesShared: Bool }
enum Demo {
    static let requests = [ActionRequest(id: "1", space: "Mast", kind: "Review", text: "Review the companion layout", detail: "Check the inbox and transcript before the next release.", options: ["Looks good", "Needs changes"], facts: ["Priority: Normal", "Screen: Needs you"], links: ["Review: https://example.com/review"], origin: "Companion design chat", priority: "blocking", item: "iOS companion", createdAt: Date().addingTimeInterval(-3600).timeIntervalSince1970 * 1000), ActionRequest(id: "2", space: "Product", kind: "Choose", text: "Choose the release notes for review", detail: "The draft is ready for a final pass.", options: ["Review draft"], facts: [], links: [], origin: "Release notes", item: "Launch", createdAt: Date().addingTimeInterval(-14400).timeIntervalSince1970 * 1000)]
    static let chats = [Chat(id: "1", title: "Companion design", space: "Mast", status: "working", harness: "claude-code", model: "Opus"), Chat(id: "2", title: "Release notes", space: "Mast", status: "awaitingInput", harness: "codex", model: "GPT-6"), Chat(id: "3", title: "Interface exploration", space: "Product", status: "idle", harness: "other", model: "Agent")]
    static let messages = [TranscriptMessage(id: "1", role: "You", text: "Prepare a compact inbox for the phone.", author: "user"), TranscriptMessage(id: "relay", role: "user", text: "[Message from Mast chat Coordinator (abc123)] Please check the latest build.", author: "relay"), TranscriptMessage(id: "2", role: "Assistant", text: "The inbox is ready for review.", author: "agent")]
}

