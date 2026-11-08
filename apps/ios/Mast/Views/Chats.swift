import SwiftUI

struct ChatsScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    private func group(_ chat: Chat) -> String { let status = chat.status.lowercased(); return status.hasPrefix("working") ? "Running" : (status.contains("awaiting") || status.contains("waiting") ? "Waiting" : "Idle") }
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 25) {
        HorizonHeader(title: "Chats")
        ForEach(["Running", "Waiting", "Idle"], id: \.self) { section in
            let chats = model.chats.filter { chat in group(chat) == section && chat.visible(showWorkers: model.showWorkers) }
            if !chats.isEmpty { VStack(alignment: .leading, spacing: 12) {
                Text(section.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme))
                ForEach(chats) { chat in if snapshot { ChatRow(chat: chat) } else { NavigationLink { ChatDetail(chat: chat, model: model) } label: { ChatRow(chat: chat) }.buttonStyle(.plain) } }
            } }
        }
    }.padding(.horizontal, 22).padding(.top, 24).padding(.bottom, 30) }.refreshable { await model.refresh() }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar) }
}
struct ChatRow: View {
    let chat: Chat
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(alignment: .top, spacing: 13) {
        Circle().fill(Horizon.provider(chat, scheme)).frame(width: 9, height: 9).padding(.top, 7)
        VStack(alignment: .leading, spacing: 6) { Text(chat.title).font(.system(size: 16, weight: .semibold)).foregroundStyle(Horizon.ink(scheme)); Text([chat.model, statusLabel].compactMap { $0 }.joined(separator: "  ·  ")).font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
        Spacer(minLength: 4)
        Text(Horizon.relativeISO(chat.lastMessageAt)).font(.system(size: 12)).foregroundStyle(Horizon.muted(scheme))
    }.padding(.vertical, 8).contentShape(Rectangle()) }
    private var statusLabel: String { let status = chat.status.lowercased(); return status.contains("stale") ? "Offline" : (status.hasPrefix("working") ? "Running" : (status.contains("awaiting") || status.contains("waiting") ? "Waiting" : (status == "idle" ? "Idle" : chat.status))) }
}
