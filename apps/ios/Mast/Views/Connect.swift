import SwiftUI

struct ConnectScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    @AppStorage("lockTimeoutSeconds") private var lockTimeout = 60
    @AppStorage("quickActions") private var quickActions = false
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        HorizonHeader(title: "Connect")
        Text("Connect to Mast on your Mac to see your chats and requests.").font(.system(size: 16)).lineSpacing(4).foregroundStyle(Horizon.muted(scheme))
        VStack(alignment: .leading, spacing: 9) { Text("BRIDGE URL").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); Group { if snapshot { Text(model.address).frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Bridge URL", text: $model.address).textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL) } }.padding(16).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 15)) }
        VStack(alignment: .leading, spacing: 9) { Text("PAIRING CODE").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); Group { if snapshot { Text("Enter code from your Mac").foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading) } else { SecureField("Enter code from your Mac", text: $model.pairingCode).textInputAutocapitalization(.never).autocorrectionDisabled() } }.padding(16).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 15)) }
        Button("Connect") { Task { await model.pair() } }.font(.system(size: 15, weight: .semibold)).foregroundStyle(scheme == .dark ? .black : .white).frame(maxWidth: .infinity).padding(15).background(Horizon.muted(scheme), in: RoundedRectangle(cornerRadius: 15)).disabled(model.pairingCode.isEmpty)
        HStack { Circle().fill(model.connection.hasPrefix("Connected") ? Color.green : Horizon.slate).frame(width: 7, height: 7); Text(model.connection); Spacer(); Button("Disconnect") { model.forget() } }.font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
        if snapshot { HStack { Text("Demo mode"); Spacer(); Text("On").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)); HStack { Text("Show worker chats"); Spacer(); Text("Off").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)) }
        else { Toggle("Demo mode", isOn: $model.demo).tint(Horizon.muted(scheme)).onChange(of: model.demo) { _, demo in if !demo { model.hasLoaded = false; model.requests = []; model.chats = [] }; Task { await model.refresh() } }.font(.system(size: 14)); Toggle("Show worker chats", isOn: $model.showWorkers).tint(Horizon.muted(scheme)).font(.system(size: 14)) }
        if snapshot { HStack { Text("Reply & resolve"); Spacer(); Text(model.replyEnabled ? "On" : "Off").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)) } else { Toggle("Reply & resolve", isOn: $model.replyEnabled).tint(Horizon.muted(scheme)).font(.system(size: 14)) }
        if !model.demo { VStack(alignment: .leading, spacing: 12) { Text("SHARED SPACES").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); if model.sharedSpaces.isEmpty { Text("No Spaces shared — run `spaces share` on your Mac").font(.system(size: 13)) }; ForEach(model.sharedSpaces) { space in Toggle(space.name, isOn: Binding(get: { !model.hiddenSpaceIds.contains(space.id) }, set: { enabled in if enabled { model.hiddenSpaceIds.remove(space.id) } else { model.hiddenSpaceIds.insert(space.id) } })).tint(Horizon.muted(scheme)).font(.system(size: 14)) } } }
        if snapshot { HStack { Text("Quick actions"); Spacer(); Text(quickActions ? "On" : "Off").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)); HStack { Text("Lock after"); Spacer(); Text("1 minute").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)) }
        else { Toggle("Quick actions", isOn: $quickActions).tint(Horizon.muted(scheme)).font(.system(size: 14)); Picker("Lock after", selection: $lockTimeout) { Text("Immediately").tag(0); Text("1 minute").tag(60); Text("5 minutes").tag(300); Text("15 minutes").tag(900) }.font(.system(size: 14)) }
    }.foregroundStyle(Horizon.ink(scheme)).padding(.horizontal, 22).padding(.top, 24) }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar) }
}
