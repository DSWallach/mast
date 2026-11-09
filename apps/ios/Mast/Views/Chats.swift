import SwiftUI

struct ChatsScreen: View {
    @Environment(\.mastStyle) private var style
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    private func group(_ chat: Chat) -> String { let status = chat.status.lowercased(); return status.hasPrefix("working") ? "Running" : (status.contains("awaiting") || status.contains("waiting") ? "Waiting" : "Idle") }
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 25) {
        HorizonHeader(title: "Chats")
        if !model.demo && model.isLoading && !model.hasLoaded { LoadingPlaceholder() }
        else if !model.demo && model.connection.hasPrefix("Disconnected") { OfflineScreen(model: model) }
        else if model.noSpacesShared { Text("No Spaces shared — run `spaces share` on your Mac").foregroundStyle(style.muted(scheme)) }
        else { if !model.visibleChats.contains(where: { $0.visible(showWorkers: model.showWorkers) && group($0) == "Running" }) { Text("No running chats").font(.system(size: 14)).foregroundStyle(style.muted(scheme)) }
        ForEach(["Running", "Waiting", "Idle"], id: \.self) { section in
            let chats = model.visibleChats.filter { chat in group(chat) == section && chat.visible(showWorkers: model.showWorkers) }
            if !chats.isEmpty { VStack(alignment: .leading, spacing: 12) {
                Text(section.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(style.muted(scheme))
                ForEach(chats) { chat in if snapshot { ChatRow(chat: chat) } else { NavigationLink { ChatDetail(chat: chat, model: model) } label: { ChatRow(chat: chat) }.buttonStyle(MastTapTargetStyle()) } }
            } }
        } }
    }.padding(.horizontal, 22).padding(.top, 24).padding(.bottom, 30) }.refreshable { await model.refresh() }.background(style.surface(scheme)).toolbar(.hidden, for: .navigationBar) }
}
struct ChatRow: View {
    @Environment(\.mastStyle) private var style
    let chat: Chat
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(alignment: .top, spacing: 13) {
        Circle().fill(style.provider(chat, scheme)).frame(width: 9, height: 9).padding(.top, 7)
        VStack(alignment: .leading, spacing: 6) { Text(chat.title).font(style.semibold(15)).foregroundStyle(style.ink(scheme)); Text([chat.model, statusLabel].compactMap { $0 }.joined(separator: "  ·  ")).font(style.ui(12)).foregroundStyle(style.muted(scheme)) }
        Spacer(minLength: 4)
        TimelineView(.periodic(from: .now, by: 60)) { _ in Text(style.relativeISO(chat.lastMessageAt)) }.font(.system(size: 12)).foregroundStyle(style.muted(scheme))
    }.frame(minHeight: 44).contentShape(Rectangle()) }
    private var statusLabel: String { let status = chat.status.lowercased(); return status.contains("stale") ? "Offline" : (status.hasPrefix("working") ? "Running" : (status.contains("awaiting") || status.contains("waiting") ? "Waiting" : (status == "idle" ? "Idle" : chat.status))) }
}
