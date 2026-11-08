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
    var body: some View { HStack(alignment: .firstTextBaseline, spacing: 10) { Text(title).font(.system(size: 34, weight: .bold)); if let count { Text("\(count)").font(.system(size: 17, weight: .medium)).foregroundStyle(Horizon.muted(scheme)) }; Spacer() }.foregroundStyle(Horizon.ink(scheme)) }
}
struct OfflineBanner: View {
    let text: String
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(spacing: 8) { Image(systemName: "wifi.slash"); Text(text).lineLimit(2); Spacer() }.font(.system(size: 13, weight: .medium)).foregroundStyle(scheme == .dark ? Color.white : Horizon.muted(scheme)).padding(.horizontal, 20).padding(.vertical, 10).background(scheme == .dark ? Color(red: 41/255, green: 52/255, blue: 60/255) : Color(red: 234/255, green: 240/255, blue: 244/255)) }
}
