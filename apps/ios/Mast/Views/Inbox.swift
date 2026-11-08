import SwiftUI

struct InboxScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    var body: some View {
        HorizonScroll { VStack(alignment: .leading, spacing: 18) {
            HorizonHeader(title: "Needs you", count: model.requests.count)
            if model.demo { Text("DEMO MODE").font(.system(size: 10, weight: .semibold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)) }
            if !model.demo && model.isLoading && !model.hasLoaded { LoadingPlaceholder() }
            else if !model.demo && model.connection.hasPrefix("Disconnected") { OfflineScreen(model: model) }
            else { ForEach(model.requests.sorted { ($0.priority == "blocking" ? 0 : 1) < ($1.priority == "blocking" ? 0 : 1) }) { request in
                if snapshot { RequestCard(request: request) } else { NavigationLink { RequestDetail(request: request) } label: { RequestCard(request: request) }.buttonStyle(.plain) }
            }
            if model.requests.isEmpty { Text("Nothing needs you").foregroundStyle(Horizon.muted(scheme)).padding(.top, 20) } }
        }.padding(.horizontal, 22).padding(.top, 24).padding(.bottom, 30) }.refreshable { await model.refresh() }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar)
    }
}
struct RequestCard: View {
    let request: ActionRequest
    @Environment(\.colorScheme) private var scheme
    var body: some View { VStack(alignment: .leading, spacing: 12) {
        HStack { Text(request.kind.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.4); Spacer(); if request.priority == "blocking" { Text("BLOCKING").font(.system(size: 10, weight: .semibold)).tracking(0.8) } }.foregroundStyle(Horizon.muted(scheme))
        Text(request.text).font(.system(size: 19, weight: .semibold)).foregroundStyle(Horizon.ink(scheme)).multilineTextAlignment(.leading)
        HStack { Text([request.space, request.item].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ›  ")); Spacer(); TimelineView(.periodic(from: .now, by: 60)) { _ in Text(Horizon.relative(request.createdAt)) } }.font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
    }.padding(19).frame(maxWidth: .infinity, alignment: .leading).background(Horizon.slate.opacity(scheme == .dark ? 0.13 : 0.10), in: RoundedRectangle(cornerRadius: 18)) }
}
