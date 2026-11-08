import SwiftUI

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
struct MarkdownReply: View {
    let text: String
    private func inline(_ text: String) -> Text {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return Text((try? AttributedString(markdown: text, options: options)) ?? AttributedString(text))
    }
    var body: some View { VStack(alignment: .leading, spacing: 10) {
        ForEach(Array(parseMarkdownBlocks(text).enumerated()), id: \.offset) { _, block in
            switch block {
            case .paragraph(let text): inline(text).font(.system(size: 17, design: .serif)).lineSpacing(5).textSelection(.enabled)
            case .heading(let level, let text): inline(text).font(.system(size: level == 1 ? 22 : 19, weight: .semibold, design: .serif)).textSelection(.enabled)
            case .item(let marker, let text): HStack(alignment: .top, spacing: 9) { Text(marker).frame(minWidth: 17, alignment: .trailing); inline(text).frame(maxWidth: .infinity, alignment: .leading) }.font(.system(size: 17, design: .serif)).lineSpacing(5)
            case .code(let code): ScrollView(.horizontal) { Text(code).font(.system(size: 13, design: .monospaced)).textSelection(.enabled).padding(14) }.background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 14))
            }
        }
    }.frame(maxWidth: .infinity, alignment: .leading) }
}

struct TranscriptContent: View {
    let messages: [TranscriptMessage]
    let chat: Chat
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 24) { ForEach(messages) { message in
        if message.author == "tool" {
            ToolActivityRow(text: message.text)
        } else if message.author == "system" || message.author == "relay" {
            SystemMessageRow(message: message)
        } else if message.author == "user" || (message.author == nil && (message.role.lowercased() == "user" || message.role.lowercased() == "you")) {
            HStack { Spacer(minLength: 45); Text(message.text).font(.system(size: 16)).padding(.horizontal, 17).padding(.vertical, 13).background(Horizon.slate.opacity(scheme == .dark ? 0.18 : 0.14), in: RoundedRectangle(cornerRadius: 19)) }
        } else {
            VStack(alignment: .leading, spacing: 9) { Text((chat.model ?? "ASSISTANT").uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.2); MarkdownReply(text: message.text) }.foregroundStyle(Horizon.provider(chat, scheme)).frame(maxWidth: .infinity, alignment: .leading)
        }
    } }.foregroundStyle(Horizon.ink(scheme)) }
}
struct ToolActivityRow: View {
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
            }.foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(13).background(Horizon.slate.opacity(0.09), in: RoundedRectangle(cornerRadius: 14))
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
    let message: TranscriptMessage
    @State private var expanded = false
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Button { expanded.toggle() } label: {
            VStack(alignment: .leading, spacing: 5) {
                Text(message.author == "relay" ? "FROM COORDINATOR" : (message.text.hasPrefix("Worker updates:") ? "WORKER UPDATE" : "MAST"))
                    .font(.system(size: 10, weight: .bold)).tracking(1)
                Text(expanded ? message.text : systemMessagePreview(message.text)).font(.system(size: 13)).lineLimit(expanded ? nil : 1).multilineTextAlignment(.leading)
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
