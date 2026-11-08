import SwiftUI
import UIKit

enum MarkdownBlock: Equatable {
    case paragraph(String), heading(Int, String), item(String, String), code(String)
}
func parseMarkdownBlocks(_ source: String) -> [MarkdownBlock] {
    var blocks: [MarkdownBlock] = [], paragraph: [String] = [], code: [String] = []
    var fenced = false
    func flush() { if !paragraph.isEmpty { blocks.append(.paragraph(paragraph.joined(separator: "\n"))); paragraph.removeAll() } }
    for line in source.components(separatedBy: "\n") {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("```") { if fenced { blocks.append(.code(code.joined(separator: "\n"))); code.removeAll() } else { flush() }; fenced.toggle(); continue }
        if fenced { code.append(line); continue }
        if trimmed.isEmpty { flush(); continue }
        let hashes = trimmed.prefix(while: { $0 == "#" }).count
        if (1...6).contains(hashes), trimmed.dropFirst(hashes).first == " " { flush(); blocks.append(.heading(hashes, String(trimmed.dropFirst(hashes + 1)))); continue }
        if let range = trimmed.range(of: #"^(?:[-*+] |[0-9]+[.)] )"#, options: .regularExpression) { flush(); let marker = String(trimmed[range]).trimmingCharacters(in: .whitespaces); blocks.append(.item(marker == "-" || marker == "*" || marker == "+" ? "•" : marker, String(trimmed[range.upperBound...]))); continue }
        paragraph.append(line)
    }
    if fenced { blocks.append(.code(code.joined(separator: "\n"))) }; flush(); return blocks
}
func linkifyBareReferences(_ text: String) -> String {
    guard let pattern = try? NSRegularExpression(pattern: #"(?<![\w(])(?:mast-artifact:[A-Za-z0-9_-]+|mast-chat:[A-Za-z0-9_-]+|https://[^\s)<>]+)"#) else { return text }
    let matches = pattern.matches(in: text, range: NSRange(text.startIndex..., in: text))
    var result = text
    for match in matches.reversed() { if let range = Range(match.range, in: result) { let link = String(result[range]); result.replaceSubrange(range, with: "[\(link)](\(link))") } }
    return result
}
func markdownInline(_ text: String, linkColor: Color) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        var value = (try? AttributedString(markdown: linkifyBareReferences(text), options: options)) ?? AttributedString(text)
        for run in value.runs {
            if run.inlinePresentationIntent?.contains(.code) == true { value[run.range].font = .system(size: 17, design: .monospaced) }
            if run.link != nil { value[run.range].foregroundColor = linkColor; value[run.range].underlineStyle = .single }
        }
        return value
}
struct MarkdownReply: View {
    @Environment(\.mastStyle) private var style
    let text: String
    let linkColor: Color
    @Environment(\.snapshotRendering) private var snapshot
    private func inline(_ text: String) -> Text {
        Text(markdownInline(text, linkColor: linkColor))
    }
    var body: some View { VStack(alignment: .leading, spacing: 10) {
        ForEach(Array(parseMarkdownBlocks(text).enumerated()), id: \.offset) { _, block in
            switch block {
            case .paragraph(let text): inline(text).font(style.serif(style.replySize)).lineSpacing(5).textSelection(.enabled)
            case .heading(let level, let text): inline(text).font(style.serif(level == 1 ? 22 : 19)).textSelection(.enabled)
            case .item(let marker, let text): HStack(alignment: .top, spacing: 9) { Text(marker).frame(minWidth: 17, alignment: .trailing); inline(text).frame(maxWidth: .infinity, alignment: .leading) }.font(style.serif(style.replySize)).lineSpacing(5)
            case .code(let code): Group { if snapshot { Text(code).font(.system(size: 13, design: .monospaced)).padding(14).frame(maxWidth: .infinity, alignment: .leading) } else { ScrollView(.horizontal) { Text(code).font(.system(size: 13, design: .monospaced)).textSelection(.enabled).padding(14) } } }.background(style.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 14))
            }
        }
    }.tint(linkColor).frame(maxWidth: .infinity, alignment: .leading) }
}

struct TranscriptContent: View {
    @Environment(\.mastStyle) private var style
    let messages: [TranscriptMessage]
    let chat: Chat
    var sendStates: [String: String] = [:]
    var retry: (TranscriptMessage) -> Void = { _ in }
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 24) { ForEach(messages) { message in
        if message.author == "tool" {
            ToolActivityRow(text: message.text)
        } else if message.author == "system" || message.author == "relay" {
            SystemMessageRow(message: message)
        } else if message.author == "user" || (message.author == nil && (message.role.lowercased() == "user" || message.role.lowercased() == "you")) {
            HStack { Spacer(minLength: 45); VStack(alignment: .trailing, spacing: 5) { Text(message.text).font(.system(size: 16)).padding(.horizontal, 17).padding(.vertical, 13).background(style.slate.opacity(scheme == .dark ? 0.18 : 0.14), in: RoundedRectangle(cornerRadius: 19)); if let state = sendStates[message.id] { if state == "Delivery unknown · Retry" { Button(state) { retry(message) }.font(.system(size: 11)).foregroundStyle(style.muted(scheme)) } else { Text(state).font(.system(size: 11)).foregroundStyle(style.muted(scheme)) } } } }
        } else {
            VStack(alignment: .leading, spacing: 9) { Text((chat.model ?? "ASSISTANT").uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.2); MarkdownReply(text: message.text, linkColor: style.replyColor(chat, scheme)) }.foregroundStyle(style.replyColor(chat, scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(style.id == .industrial ? 12 : 0).overlay { if style.id == .industrial { Rectangle().stroke(style.rule(scheme), lineWidth: 1) } }
        }
    } }.foregroundStyle(style.ink(scheme)) }
}
struct ToolActivityRow: View {
    @Environment(\.mastStyle) private var style
    let text: String
    @State private var expanded = false
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        let tools = text.components(separatedBy: "\n").filter { !$0.isEmpty }
        let commands = tools.allSatisfy { $0.hasPrefix("exec:") }
        Button { expanded.toggle() } label: {
            VStack(alignment: .leading, spacing: 8) {
                Label(commands ? "Ran \(tools.count) \(tools.count == 1 ? "command" : "commands")" : "Used \(tools.count) \(tools.count == 1 ? "tool" : "tools")", systemImage: expanded ? "chevron.down" : "chevron.right").font(.system(size: 12, weight: .medium))
                if expanded { ForEach(Array(tools.enumerated()), id: \.offset) { _, tool in Text(tool).font(.system(size: 12, design: .monospaced)).textSelection(.enabled) } }
            }.foregroundStyle(style.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(13).background(style.slate.opacity(0.09), in: RoundedRectangle(cornerRadius: 14))
        }.buttonStyle(.plain)
    }
}
func systemMessagePreview(_ text: String) -> String {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if (trimmed.hasPrefix("[Message from Mast chat ") || trimmed.hasPrefix("[Mast system message,")),
       let end = trimmed.firstIndex(of: "]") {
        return String(trimmed[trimmed.index(after: end)...]).trimmingCharacters(in: .whitespacesAndNewlines)
    }
    if trimmed.hasPrefix("Re: "), let expression = try? NSRegularExpression(pattern: #"^Re:\s*[\"“].*?[\"”]:\s*"#),
       let match = expression.firstMatch(in: trimmed, range: NSRange(trimmed.startIndex..., in: trimmed)),
       let range = Range(match.range, in: trimmed) {
        return String(trimmed[range.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
    }
    return trimmed
}
struct SystemMessageRow: View {
    @Environment(\.mastStyle) private var style
    let message: TranscriptMessage
    @State private var expanded = false
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Button { expanded.toggle() } label: { VStack(alignment: .leading, spacing: 5) {
                Text(message.author == "relay" ? "FROM COORDINATOR" : (message.text.hasPrefix("Worker updates:") ? "WORKER UPDATE" : "MAST"))
                    .font(.system(size: 10, weight: .bold)).tracking(1)
                if !expanded { Text(systemMessagePreview(message.text)).font(.system(size: 13)).lineLimit(1).multilineTextAlignment(.leading) }
            }.frame(maxWidth: .infinity, alignment: .leading) }.buttonStyle(.plain)
            if expanded { MarkdownReply(text: message.text, linkColor: style.action(scheme)) }
        }.foregroundStyle(style.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(13).background(style.slate.opacity(0.09), in: RoundedRectangle(cornerRadius: 14))
    }
}
struct ChatDetail: View {
    @Environment(\.mastStyle) private var style
    let chat: Chat
    @ObservedObject var model: Companion
    @State private var text = ""
    @State private var notice = ""
    @State private var messages: [TranscriptMessage]
    @State private var sending = false
    @State private var localMessages: [TranscriptMessage] = []
    @State private var sendStates: [String: String] = [:]
    @AppStorage("quickActions") private var quickActions = false
    @State private var confirmMessage: TranscriptMessage?
    @State private var showConfirmation = false
    @State private var confirmStop = false
    @State private var olderRemaining = 0
    @State private var nextOffset = 0
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    init(chat: Chat, model: Companion, initialMessages: [TranscriptMessage] = []) {
        self.chat = chat; self.model = model; _messages = State(initialValue: initialMessages)
    }
    private func send(_ message: TranscriptMessage) {
        sending = true; sendStates[message.id] = "Sending"
        Task {
            do {
                let result = try await model.send(chat, text: message.text)
                sendStates[message.id] = result == "Accepted" ? "Sent" : result == "Queued" ? "Queued" : "Demo"
                if result == "Accepted" || result == "Queued" { UIImpactFeedbackGenerator(style: .light).impactOccurred() }
                if result == "Accepted" || result == "Queued", let page = try? await model.transcript(chat) {
                    var refreshed = page.messages
                    let previousIDs = Set(messages.map(\.id))
                    if let duplicate = refreshed.lastIndex(where: { !previousIDs.contains($0.id) && $0.author == "user" && $0.text == message.text }) { refreshed.remove(at: duplicate) }
                    messages = refreshed; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset
                }
            } catch {
                sendStates[message.id] = "Delivery unknown · Retry"
                notice = "Check the chat before retrying; the message may have arrived."
            }
            sending = false
        }
    }
    private func requestSend(_ message: TranscriptMessage) {
        if requiresConfirmation(quickActions: quickActions) { confirmMessage = message; showConfirmation = true } else { confirmAndSend(message) }
    }
    private func confirmAndSend(_ message: TranscriptMessage) {
        guard model.canView(spaceId: chat.spaceId) else { return }
        if !localMessages.contains(where: { $0.id == message.id }) { localMessages.append(message); text = "" }
        notice = ""; send(message)
    }
    private func stop() {
        guard model.canView(spaceId: chat.spaceId) else { return }
        Task { do { try await model.stop(chat); notice = "Stop requested" } catch { notice = "Unable to stop the chat." } }
    }
    var body: some View { Group { if !model.canView(spaceId: chat.spaceId) { Text("Space is unavailable or no longer shared.").foregroundStyle(style.muted(scheme)).padding(22).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).background(style.surface(scheme)) } else { VStack(spacing: 0) {
        ScrollViewReader { proxy in
            HorizonScroll { VStack(alignment: .leading, spacing: 20) {
                if olderRemaining > 0 { Button("Load earlier") { Task { let first = messages.first?.id; do { let page = try await model.transcript(chat, offset: nextOffset); messages = page.messages + messages; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset; if let first { proxy.scrollTo(first, anchor: .top) } } catch { notice = "Earlier messages unavailable." } } }.font(.system(size: 13)).foregroundStyle(style.muted(scheme)) }
                TranscriptContent(messages: messages + localMessages, chat: chat, sendStates: sendStates, retry: requestSend)
                Color.clear.frame(height: 1).id("transcript-bottom")
            }.padding(22).frame(maxWidth: .infinity, alignment: .leading) }
            .task { do { let page = try await model.transcript(chat); messages = page.messages; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset; DispatchQueue.main.async { proxy.scrollTo("transcript-bottom", anchor: .bottom) } } catch { notice = "Transcript unavailable or stale." } }
            .onAppear { model.activeChatId = chat.id }.onDisappear { if model.activeChatId == chat.id { model.activeChatId = nil } }
            .onChange(of: model.transcriptRevision) { _, _ in Task { if let page = try? await model.transcript(chat) { messages = page.messages; olderRemaining = page.olderRemaining; nextOffset = page.nextOffset; DispatchQueue.main.async { proxy.scrollTo("transcript-bottom", anchor: .bottom) } } } }
            .onChange(of: sending) { _, _ in DispatchQueue.main.async { proxy.scrollTo("transcript-bottom", anchor: .bottom) } }
        }
        VStack(alignment: .leading, spacing: 8) {
            if !notice.isEmpty { Text(notice).font(.system(size: 12)).foregroundStyle(style.muted(scheme)) }
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 8) {
                    Image(systemName: style.providerSymbol(chat)).font(.system(size: 16)).foregroundStyle(.white).padding(8).background(Color(red: 24/255, green: 48/255, blue: 40/255), in: RoundedRectangle(cornerRadius: 8))
                    if snapshot { Text("Do anything…").frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Do anything…", text: $text, axis: .vertical).lineLimit(3...6) }
                }.font(style.ui(15)).foregroundStyle(style.id == .horizon ? Color.black.opacity(0.8) : style.ink(scheme)).frame(maxWidth: .infinity, minHeight: 88, alignment: .topLeading).padding(16).background(style.composer(chat, scheme), in: RoundedRectangle(cornerRadius: style.panelRadius)).overlay { if style.id == .industrial { Rectangle().stroke(style.rule(scheme), lineWidth: 1) } }.overlay(alignment: .topLeading) { if style.id == .horizon { Image(systemName: style.providerSymbol(chat)).font(.system(size: 10)).foregroundStyle(.black).padding(6).background(style.identity(chat), in: Capsule()).offset(x: 12, y: -10) } }
                HStack(spacing: 8) {
                    Label("Attach", systemImage: "paperclip").accessibilityLabel("Attachments unavailable").opacity(0.6)
                    Spacer()
                    Label(chat.model ?? "Assistant", systemImage: style.providerSymbol(chat)).lineLimit(1).padding(.horizontal, 9).padding(.vertical, 7).background(style.slate.opacity(0.13), in: Capsule())
                    Text("Ask").opacity(0.6)
                    if (model.chats.first(where: { $0.id == chat.id })?.status ?? chat.status).lowercased().hasPrefix("working") { Button("STOP") { if requiresConfirmation(quickActions: quickActions) { confirmStop = true } else { stop() } } }
                    Button("SEND") { requestSend(TranscriptMessage(id: UUID().uuidString, role: "user", text: text, author: "user")) }.disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty).foregroundStyle(style.surface(scheme)).padding(.horizontal, 14).padding(.vertical, 8).background(style.action(scheme), in: RoundedRectangle(cornerRadius: style.chipRadius))
                }.font(style.semibold(10)).foregroundStyle(style.muted(scheme)).padding(.top, 8)
            }
        }.padding(.horizontal, 18).padding(.bottom, 12).padding(.top, 8)
    }.background(style.surface(scheme)) } }.navigationTitle(model.canView(spaceId: chat.spaceId) ? chat.title : "Chat").navigationBarTitleDisplayMode(.inline).confirmationDialog("Stop this chat?", isPresented: $confirmStop, titleVisibility: .visible) { Button("Stop") { stop() }; Button("Cancel", role: .cancel) {} }.confirmationDialog("Send this message?", isPresented: $showConfirmation, titleVisibility: .visible) { Button("Send") { if let message = confirmMessage { confirmAndSend(message) }; confirmMessage = nil }; Button("Cancel", role: .cancel) { confirmMessage = nil } } message: { if let message = confirmMessage { Text(message.text) } } }
}
