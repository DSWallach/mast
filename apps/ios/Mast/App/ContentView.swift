import SwiftUI

struct ContentView: View {
    @StateObject private var model = Companion()
    @Environment(\.scenePhase) private var phase
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(spacing: 0) {
        if model.connection.hasPrefix("Disconnected") { OfflineBanner(text: model.connection) }
        TabView {
            NavigationStack { InboxScreen(model: model) }.tabItem { Label("Needs you", systemImage: "tray") }
            NavigationStack { ChatsScreen(model: model) }.tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
            NavigationStack { ConnectScreen(model: model) }.tabItem { Label("Connect", systemImage: "link") }
        }.tint(Horizon.muted(scheme)).toolbarBackground(Horizon.surface(scheme), for: .tabBar).toolbarBackground(.visible, for: .tabBar)
    }.background(Horizon.surface(scheme)).onChange(of: phase) { _, value in if value == .active { Task { await model.refresh() } } } }
}
