import SwiftUI

struct RequestDetail: View {
    let request: ActionRequest
    @Environment(\.colorScheme) private var scheme
    private func heading(_ text: String) -> some View { Text(text.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)) }
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        Text(request.kind.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme))
        Text(request.text).font(.system(size: 30, weight: .bold)).fixedSize(horizontal: false, vertical: true)
        Text([request.space, request.item].compactMap { $0 }.joined(separator: "  ›  ")).font(.system(size: 14)).foregroundStyle(Horizon.muted(scheme))
        if !request.detail.isEmpty { Text(request.detail).font(.system(size: 17)).lineSpacing(4) }
        if !request.options.isEmpty { VStack(alignment: .leading, spacing: 10) { heading("Options"); ForEach(request.options, id: \.self) { Text($0).font(.system(size: 14, weight: .medium)).padding(.horizontal, 14).padding(.vertical, 9).background(Horizon.slate.opacity(0.14), in: Capsule()) } } }
        if !request.facts.isEmpty { VStack(alignment: .leading, spacing: 12) { heading("Facts"); ForEach(request.facts, id: \.self) { fact in let parts = fact.split(separator: ":", maxSplits: 1).map(String.init); HStack(alignment: .top) { Text(parts[0]).foregroundStyle(Horizon.muted(scheme)); Spacer(); if parts.count > 1 { Text(parts[1].trimmingCharacters(in: .whitespaces)).multilineTextAlignment(.trailing) } }.font(.system(size: 14)) } } }
        if !request.links.isEmpty { VStack(alignment: .leading, spacing: 10) { heading("Links"); ForEach(request.links, id: \.self) { Text($0).font(.system(size: 14)).textSelection(.enabled) } } }
        VStack(alignment: .leading, spacing: 8) { heading("Origin"); Text(request.origin).font(.system(size: 14)) }
        Text("Reply & resolve is unavailable until delivery is verified.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)).padding(.top, 8)
    }.foregroundStyle(Horizon.ink(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(22) }.background(Horizon.surface(scheme)).navigationBarTitleDisplayMode(.inline)
    }
}
