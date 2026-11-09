import SwiftUI

private struct SnapshotRenderingKey: EnvironmentKey { static let defaultValue = false }
extension EnvironmentValues { var snapshotRendering: Bool { get { self[SnapshotRenderingKey.self] } set { self[SnapshotRenderingKey.self] = newValue } } }
struct HorizonScroll<Content: View>: View {
    @Environment(\.mastStyle) private var style
    @Environment(\.snapshotRendering) private var snapshot
    @ViewBuilder var content: Content
    var body: some View { if snapshot { content.fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top) } else { ScrollView { content } } }
}
struct HorizonHeader: View {
    @Environment(\.mastStyle) private var style
    let title: String
    var count: Int? = nil
    @Environment(\.colorScheme) private var scheme
    @ScaledMetric(relativeTo: .largeTitle) private var titleSize = 34
    var body: some View { VStack(alignment: .leading, spacing: 16) {
        Text("Mast").font(style.serif(26)).foregroundStyle(style.muted(scheme)).accessibilityAddTraits(.isHeader)
        HStack(alignment: .firstTextBaseline, spacing: 10) { Text(title).font(style.semibold(24)); if let count { Text("\(count)").font(style.ui(14)).foregroundStyle(style.muted(scheme)) }; Spacer() }.foregroundStyle(style.ink(scheme))
    } }
}
struct OfflineBanner: View {
    @Environment(\.mastStyle) private var style
    let text: String
    @Environment(\.colorScheme) private var scheme
    var body: some View { HStack(spacing: 8) { Image(systemName: "wifi.slash"); Text(text).lineLimit(2); Spacer() }.font(.system(size: 13, weight: .medium)).foregroundStyle(scheme == .dark ? Color.white : style.muted(scheme)).padding(.horizontal, 20).padding(.vertical, 10).background(scheme == .dark ? Color(red: 41/255, green: 52/255, blue: 60/255) : Color(red: 234/255, green: 240/255, blue: 244/255)) }
}
struct LoadingPlaceholder: View {
    @Environment(\.mastStyle) private var style
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 14) { ForEach(0..<3) { _ in RoundedRectangle(cornerRadius: 18).fill(style.slate.opacity(scheme == .dark ? 0.16 : 0.10)).frame(height: 104) } }.accessibilityLabel("Loading") }
}
struct OfflineScreen: View {
    @Environment(\.mastStyle) private var style
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 16) {
        Image(systemName: "wifi.slash").font(.system(size: 25)).foregroundStyle(style.muted(scheme))
        Text("Mac is offline").font(.system(size: 24, weight: .semibold)).foregroundStyle(style.ink(scheme))
        if let date = model.lastUpdated { Text("Last updated \(date.formatted(.relative(presentation: .named)))").font(.system(size: 14)).foregroundStyle(style.muted(scheme)) }
        else { Text("Connect to your Mac and try again.").font(.system(size: 14)).foregroundStyle(style.muted(scheme)) }
        Button { Task { await model.refresh() } } label: { Text("Retry").font(.system(size: 14, weight: .semibold)).foregroundStyle(style.muted(scheme)).padding(.horizontal, 18).frame(minHeight: 44).background(style.slate.opacity(0.15), in: Capsule()).contentShape(Rectangle()) }.buttonStyle(MastTapTargetStyle())
    }.frame(maxWidth: .infinity, alignment: .leading).padding(.top, 30) }
}

struct RequestLinkView: View {
    @Environment(\.openURL) private var openURL
    let link: RequestLink
    var body: some View {
        let target = link.target
        let destination: String = switch target.type {
        case "artifact": "mast-artifact:\(target.id ?? "")"
        case "chat": "mast-chat:\(target.id ?? "")"
        case "url", "pr": target.url ?? ""
        default: "mast-open-on-mac:"
        }
        Button(link.label) { if let url = URL(string: destination) { openURL(url) } }.buttonStyle(MastTapTargetStyle()).underline()
    }
}

/// Keeps the visual label inside the tappable 44-point minimum target.
struct MastTapTargetStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.frame(minWidth: 44, minHeight: 44).contentShape(Rectangle()).opacity(configuration.isPressed ? 0.7 : 1)
    }
}
