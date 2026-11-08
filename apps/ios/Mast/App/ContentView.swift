import SwiftUI

struct ContentView: View {
    @StateObject private var model = Companion()
    @StateObject private var appLock = AppLock()
    @AppStorage("lockTimeoutSeconds") private var lockTimeout = 60
    @Environment(\.scenePhase) private var phase
    @Environment(\.colorScheme) private var scheme
    var body: some View { ZStack { VStack(spacing: 0) {
        if model.connection.hasPrefix("Disconnected") { OfflineBanner(text: model.connection) }
        if !model.demo { Text(model.liveStatus).font(.system(size: 10)).foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .trailing).padding(.horizontal, 16) }
        TabView {
            NavigationStack { InboxScreen(model: model) }.tabItem { Label("Needs you", systemImage: "tray") }
            NavigationStack { ChatsScreen(model: model) }.tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
            NavigationStack { ConnectScreen(model: model) }.tabItem { Label("Connect", systemImage: "link") }
        }.tint(Horizon.muted(scheme)).toolbarBackground(Horizon.surface(scheme), for: .tabBar).toolbarBackground(.visible, for: .tabBar)
    }.background(Horizon.surface(scheme)).opacity(appLock.obscured ? 0 : 1).accessibilityHidden(appLock.obscured)
    if appLock.obscured { LockScreen(authenticating: appLock.authenticating, noPasscode: appLock.noPasscode, unlock: { appLock.unlock() }, continueWithoutPasscode: { appLock.continueWithoutPasscode() }) }
    }.onAppear { if phase == .active { appLock.foreground(timeout: TimeInterval(lockTimeout)) } }
        .onChange(of: phase) { _, value in
            switch value { case .active: appLock.foreground(timeout: TimeInterval(lockTimeout)); case .inactive: appLock.inactive(); case .background: appLock.background(); @unknown default: appLock.inactive() }
        }.onChange(of: appLock.obscured) { _, obscured in if !obscured { Task { await model.refresh() } } }
        .task(id: "\(appLock.obscured)-\(model.demo)-\(model.activeChatId ?? "")") { if !appLock.obscured { await model.watchEvents() } }
    }
}
struct LockScreen: View {
    let authenticating: Bool
    var noPasscode = false
    let unlock: () -> Void
    var continueWithoutPasscode: () -> Void = {}
    @Environment(\.colorScheme) private var scheme
    var body: some View { ZStack { Horizon.surface(scheme).ignoresSafeArea(); VStack(spacing: 18) {
        Image(systemName: "lock.fill").font(.system(size: 28)).foregroundStyle(Horizon.muted(scheme))
        Text("Mast is locked").font(.system(size: 26, weight: .semibold)).foregroundStyle(Horizon.ink(scheme))
        Button(authenticating ? "Unlocking…" : "Unlock") { unlock() }.disabled(authenticating).foregroundStyle(Horizon.muted(scheme))
        if noPasscode { Text("This device has no passcode. Set one for a private lock, or continue without it.").font(.system(size: 13)).multilineTextAlignment(.center).foregroundStyle(Horizon.muted(scheme)); Button("Continue without lock") { continueWithoutPasscode() }.foregroundStyle(Horizon.muted(scheme)) }
        else { Text("Use Face ID or your device passcode.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
    }.padding(24) } }
}
