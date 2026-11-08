import SwiftUI

struct TranscriptContent: View {
    let messages: [TranscriptMessage]
    let chat: Chat
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 24) { ForEach(messages) { message in
        if message.author == "system" || message.author == "relay" {
            SystemMessageRow(message: message)
        } else if message.author == "user" || (message.author == nil && (message.role.lowercased() == "user" || message.role.lowercased() == "you")) {
            HStack { Spacer(minLength: 45); Text(message.text).font(.system(size: 16)).padding(.horizontal, 17).padding(.vertical, 13).background(Horizon.slate.opacity(scheme == .dark ? 0.18 : 0.14), in: RoundedRectangle(cornerRadius: 19)) }
        } else {
            VStack(alignment: .leading, spacing: 9) { Text((chat.model ?? "ASSISTANT").uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.2); Text(message.text).font(.system(size: 17, design: .serif)).lineSpacing(5).textSelection(.enabled) }.foregroundStyle(Horizon.provider(chat, scheme)).frame(maxWidth: .infinity, alignment: .leading)
        }
    } }.foregroundStyle(Horizon.ink(scheme)) }
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
