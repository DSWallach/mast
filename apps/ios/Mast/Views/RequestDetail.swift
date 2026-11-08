import SwiftUI

struct RequestDetail: View {
    let request: ActionRequest
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    @State private var text = ""
    @State private var pending: PendingDecision?
    @State private var notice = ""
    @State private var sending = false
    @AppStorage("quickActions") private var quickActions = false
    @State private var confirmation: PendingDecision?
    @State private var showConfirmation = false
    private func confirm(outcome: String, text: String? = nil, optionId: String? = nil) {
        if requiresConfirmation(quickActions: quickActions) { confirmation = PendingDecision(key: "", outcome: outcome, text: text, optionId: optionId); showConfirmation = true }
        else { submit(outcome: outcome, text: text, optionId: optionId) }
    }
    private func submit(outcome: String, text: String? = nil, optionId: String? = nil) {
        sending = true; notice = "Sending…"
        Task {
            do {
                var result = try await model.answer(request.id, outcome: outcome, text: text, optionId: optionId)
                pending = result
                if result.phase == "sent" {
                    notice = result.resolveOnly == true ? "No chat message sent · resolving…" : "Sent · resolving…"
                    result = try await model.answer(request.id, outcome: outcome, text: text, optionId: optionId)
                    pending = result
                }
                switch result.phase {
                case "resolved": notice = model.demo ? "Demo · no request changed" : (result.resolveOnly == true ? "Resolved · no chat message sent" : "Resolved")
                case "sent", "resolving": notice = result.resolveOnly == true ? "No chat message sent; resolution pending. Retry only resolves." : "Sent; resolution pending. Retry only resolves."
                case "unknown": notice = "Delivery unknown. Check the chat before taking further action."
                case "conflict": notice = "The request was resolved elsewhere. Check the chat."
                default: notice = "Delivery pending. Retry safely."
                }
            } catch {
                pending = try? model.pendingDecision(request.id)
                notice = "Could not confirm delivery. Retry uses the same operation."
            }
            sending = false
        }
    }
    private var canAnswer: Bool { model.replyEnabled && !sending && pending == nil && !request.text.hasPrefix("Review proposed approval rule: ") && ["ask", "approve", "choose"].contains(request.kind.lowercased()) }
    private func heading(_ text: String) -> some View { Text(text.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)) }
    var body: some View { Group { if !model.canView(spaceId: request.spaceId) { Text("Space is unavailable or no longer shared.").foregroundStyle(Horizon.muted(scheme)).padding(22).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).background(Horizon.surface(scheme)) } else { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        Text(request.kind.uppercased()).font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme))
        Text(request.text).font(.system(size: 30, weight: .bold)).fixedSize(horizontal: false, vertical: true)
        Text([request.space, request.item].compactMap { $0 }.joined(separator: "  ›  ")).font(.system(size: 14)).foregroundStyle(Horizon.muted(scheme))
        if !request.detail.isEmpty { Text(request.detail).font(.system(size: 17)).lineSpacing(4) }
        if !request.options.isEmpty { VStack(alignment: .leading, spacing: 10) { heading("Options"); if request.kind.lowercased() == "choose", let choices = request.optionChoices { ForEach(choices) { option in Button(option.label) { confirm(outcome: "chosen", optionId: option.id) }.disabled(!canAnswer).font(.system(size: 14, weight: .medium)).padding(.horizontal, 14).padding(.vertical, 9).background(Horizon.slate.opacity(0.14), in: Capsule()) } } else { ForEach(request.options, id: \.self) { Text($0).font(.system(size: 14, weight: .medium)).padding(.horizontal, 14).padding(.vertical, 9).background(Horizon.slate.opacity(0.14), in: Capsule()) } } } }
        if !request.facts.isEmpty { VStack(alignment: .leading, spacing: 12) { heading("Facts"); ForEach(request.facts, id: \.self) { fact in let parts = fact.split(separator: ":", maxSplits: 1).map(String.init); HStack(alignment: .top) { Text(parts[0]).foregroundStyle(Horizon.muted(scheme)); Spacer(); if parts.count > 1 { Text(parts[1].trimmingCharacters(in: .whitespaces)).multilineTextAlignment(.trailing) } }.font(.system(size: 14)) } } }
        if !request.links.isEmpty { VStack(alignment: .leading, spacing: 10) { heading("Links"); ForEach(request.links, id: \.self) { Text($0).font(.system(size: 14)).textSelection(.enabled) } } }
        VStack(alignment: .leading, spacing: 8) { heading("Origin"); Text(request.origin).font(.system(size: 14)) }
        if model.replyEnabled {
            if request.text.hasPrefix("Review proposed approval rule: ") { Text("Review approval rules on your Mac.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
            if request.kind.lowercased() == "ask" && pending == nil { VStack(alignment: .leading, spacing: 10) { heading("Your reply"); Group { if snapshot { Text("Write a reply").foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Write a reply", text: $text, axis: .vertical) } }.padding(14).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 14)); Button("Reply & resolve") { confirm(outcome: "answered", text: text.trimmingCharacters(in: .whitespacesAndNewlines)) }.font(.system(size: 14, weight: .semibold)).padding(.horizontal, 16).padding(.vertical, 10).background(Horizon.slate.opacity(0.14), in: Capsule()).disabled(!canAnswer || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) } }
            if request.kind.lowercased() == "approve" && pending == nil { HStack(spacing: 10) { Button("Approve") { confirm(outcome: "approved") }.padding(.horizontal, 16).padding(.vertical, 10).background(Horizon.slate.opacity(0.14), in: Capsule()); Button("Decline") { confirm(outcome: "declined") }.padding(.horizontal, 16).padding(.vertical, 10).background(Horizon.slate.opacity(0.14), in: Capsule()) }.font(.system(size: 14, weight: .semibold)).disabled(!canAnswer) }
            if let pending, ["unconfirmed", "prepared", "idleDispatch", "sent", "resolving"].contains(pending.phase) { Button(pending.phase == "sent" || pending.phase == "resolving" ? "Retry resolution" : "Retry") { confirm(outcome: pending.outcome, text: pending.text, optionId: pending.optionId) }.disabled(sending) }
            if !notice.isEmpty { Text(notice).font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
        } else { Text("Reply & resolve is off in Connect.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme)) }
    }.font(Horizon.ui(14)).foregroundStyle(Horizon.ink(scheme)).frame(maxWidth: .infinity, alignment: .leading).padding(22) }.background(Horizon.surface(scheme)) } }.navigationBarTitleDisplayMode(.inline).confirmationDialog("Confirm this decision?", isPresented: $showConfirmation, titleVisibility: .visible) { Button("Confirm") { if let choice = confirmation, model.canView(spaceId: request.spaceId) { submit(outcome: choice.outcome, text: choice.text, optionId: choice.optionId) }; confirmation = nil }; Button("Cancel", role: .cancel) { confirmation = nil } } message: { if let choice = confirmation { Text(choice.text ?? request.optionChoices?.first(where: { $0.id == choice.optionId })?.label ?? choice.outcome.capitalized) } }.task { do { pending = try model.pendingDecision(request.id); if let pending { notice = pending.phase == "resolved" ? (pending.resolveOnly == true ? "Resolved · no chat message sent" : "Resolved") : pending.phase == "unknown" ? "Delivery unknown. Check the chat before taking further action." : pending.phase == "conflict" ? "The request was resolved elsewhere. Check the chat." : "Reply pending. Retry with the same operation." } } catch { notice = "Unable to read the saved reply; sending is disabled."; sending = true } }
    }
}
