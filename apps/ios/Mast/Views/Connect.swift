import SwiftUI

struct ConnectScreen: View {
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        HorizonHeader(title: "Connect")
        Text("Connect to Mast on your Mac to see your chats and requests.").font(.system(size: 16)).lineSpacing(4).foregroundStyle(Horizon.muted(scheme))
        VStack(alignment: .leading, spacing: 9) { Text("BRIDGE URL").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); Group { if snapshot { Text(model.address).frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Bridge URL", text: $model.address).textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL) } }.padding(16).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 15)) }
        VStack(alignment: .leading, spacing: 9) { Text("PAIRING CODE").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(Horizon.muted(scheme)); Group { if snapshot { Text("Enter code from your Mac").foregroundStyle(Horizon.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading) } else { SecureField("Enter code from your Mac", text: $model.pairingCode).textInputAutocapitalization(.never).autocorrectionDisabled() } }.padding(16).background(Horizon.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: 15)) }
        Button("Connect") { Task { await model.pair() } }.font(.system(size: 15, weight: .semibold)).foregroundStyle(scheme == .dark ? .black : .white).frame(maxWidth: .infinity).padding(15).background(Horizon.muted(scheme), in: RoundedRectangle(cornerRadius: 15)).disabled(model.pairingCode.isEmpty)
        HStack { Circle().fill(model.connection.hasPrefix("Connected") ? Color.green : Horizon.slate).frame(width: 7, height: 7); Text(model.connection); Spacer(); Button("Disconnect") { model.forget() } }.font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
        if snapshot { HStack { Text("Demo mode"); Spacer(); Text("On").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)); HStack { Text("Show worker chats"); Spacer(); Text("Off").foregroundStyle(Horizon.muted(scheme)) }.font(.system(size: 14)) }
        else { Toggle("Demo mode", isOn: $model.demo).tint(Horizon.muted(scheme)).onChange(of: model.demo) { _, demo in if !demo { model.hasLoaded = false; model.requests = []; model.chats = [] }; Task { await model.refresh() } }.font(.system(size: 14)); Toggle("Show worker chats", isOn: $model.showWorkers).tint(Horizon.muted(scheme)).font(.system(size: 14)) }
        Text("Reply & resolve is unavailable until delivery is verified.").font(.system(size: 13)).foregroundStyle(Horizon.muted(scheme))
    }.foregroundStyle(Horizon.ink(scheme)).padding(.horizontal, 22).padding(.top, 24) }.background(Horizon.surface(scheme)).toolbar(.hidden, for: .navigationBar) }
}
