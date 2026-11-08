import SwiftUI

private struct SnapshotRenderingKey: EnvironmentKey { static let defaultValue = false }
extension EnvironmentValues { var snapshotRendering: Bool { get { self[SnapshotRenderingKey.self] } set { self[SnapshotRenderingKey.self] = newValue } } }
struct HorizonScroll<Content: View>: View {
    @Environment(\.snapshotRendering) private var snapshot
    @ViewBuilder var content: Content
    var body: some View { if snapshot { content.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top) } else { ScrollView { content } } }
}
struct HorizonHeader: View {
    let title: String
    var count: Int? = nil
    @Environment(\.colorScheme) private var scheme
    @ScaledMetric(relativeTo: .largeTitle) private var titleSize = 34
    var body: some View { HStack(alignment: .firstTextBaseline, spacing: 10) { Text(title).font(.system(size: titleSize, weight: .bold)); if let count { Text("\(count)").font(.system(size: 17, weight: .medium)).foregroundStyle(Horizon.muted(scheme)) }; Spacer() }.foregroundStyle(Horizon.ink(scheme)) }
}
struct OfflineBanner: View {
    let text: String
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(spacing: 8) { Image(systemName: "wifi.slash"); Text(text).lineLimit(2); Spacer() }.font(.system(size: 13, weight: .medium)).foregroundStyle(scheme == .dark ? Color.white : Horizon.muted(scheme)).padding(.horizontal, 20).padding(.vertical, 10).background(scheme == .dark ? Color(red: 41/255, green: 52/255, blue: 60/255) : Color(red: 234/255, green: 240/255, blue: 244/255)) }
}
struct LoadingPlaceholder: View {
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 14) { ForEach(0..<3) { _ in RoundedRectangle(cornerRadius: 18).fill(Horizon.slate.opacity(scheme == .dark ? 0.16 : 0.10)).frame(height: 104) } }.accessibilityLabel("Loading") }
}
struct OfflineScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 16) {
        Image(systemName: "wifi.slash").font(.system(size: 25)).foregroundStyle(Horizon.muted(scheme))
        Text("Mac is offline").font(.system(size: 24, weight: .semibold)).foregroundStyle(Horizon.ink(scheme))
        if let date = model.lastUpdated { Text("Last updated \(date.formatted(.relative(presentation: .named)))").font(.system(size: 14)).foregroundStyle(Horizon.muted(scheme)) }
        else { Text("Connect to your Mac and try again.").font(.system(size: 14)).foregroundStyle(Horizon.muted(scheme)) }
        Button("Retry") { Task { await model.refresh() } }.font(.system(size: 14, weight: .semibold)).foregroundStyle(Horizon.muted(scheme)).padding(.horizontal, 18).padding(.vertical, 10).background(Horizon.slate.opacity(0.15), in: Capsule())
    }.frame(maxWidth: .infinity, alignment: .leading).padding(.top, 30) }
}
