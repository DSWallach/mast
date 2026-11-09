import SwiftUI
import AVFoundation
import UIKit

struct ConnectScreen: View {
    @Environment(\.mastStyle) private var style
    @ObservedObject var model: Companion
    @Environment(\.colorScheme) private var scheme
    @Environment(\.snapshotRendering) private var snapshot
    @AppStorage("lockTimeoutSeconds") private var lockTimeout = 0
    @AppStorage("quickActions") private var quickActions = false
    @AppStorage("mastStyleChoice") private var styleChoice = "match"
    @AppStorage("mastAppearanceChoice") private var appearanceChoice = "system"
    @State private var scanning = false
    @State private var forgetError = ""
    @State private var confirmForget = false
    @State private var confirmChange = false
    @State private var scanAfterChange = false
    @State private var scanError = ""
    var body: some View { HorizonScroll { VStack(alignment: .leading, spacing: 24) {
        HorizonHeader(title: "Connect")
        Text("Connect to Mast on your Mac to see your chats and requests.").font(.system(size: 16)).lineSpacing(4).foregroundStyle(style.muted(scheme))
        VStack(alignment: .leading, spacing: 9) { Text("BRIDGE URL").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(style.muted(scheme)); Group { if snapshot { Text(model.address).frame(maxWidth: .infinity, alignment: .leading) } else { TextField("Bridge URL", text: $model.address).frame(minHeight: 44).contentShape(Rectangle()).disabled(model.isPaired).textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL) } }.padding(16).background(style.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: style.controlRadius)) }
        VStack(alignment: .leading, spacing: 9) { Text("PAIRING CODE").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(style.muted(scheme)); Group { if snapshot { Text("Enter code from your Mac").foregroundStyle(style.muted(scheme)).frame(maxWidth: .infinity, alignment: .leading) } else { SecureField("Enter code from your Mac", text: $model.pairingCode).frame(minHeight: 44).contentShape(Rectangle()).textInputAutocapitalization(.never).autocorrectionDisabled() } }.padding(16).background(style.slate.opacity(0.12), in: RoundedRectangle(cornerRadius: style.controlRadius)) }
        if model.isPaired { Button("Change bridge") { scanAfterChange = false; confirmChange = true }.foregroundStyle(style.muted(scheme)) }
        if !snapshot { Button("Scan QR") { if model.isPaired { scanAfterChange = true; confirmChange = true } else { scanning = true } }.foregroundStyle(style.muted(scheme)); if !scanError.isEmpty { Text(scanError).font(.system(size: 12)).foregroundStyle(style.muted(scheme)) } }
        Button { Task { await model.pair() } } label: {
            HStack { if model.pairingInProgress { ProgressView().tint(scheme == .dark ? .black : .white) }; Text(model.pairingInProgress ? "Connecting…" : "Connect") }
                .font(.system(size: 15, weight: .semibold)).foregroundStyle(scheme == .dark ? .black : .white)
                .frame(maxWidth: .infinity, minHeight: 52).background(style.muted(scheme), in: RoundedRectangle(cornerRadius: style.controlRadius)).contentShape(Rectangle())
        }.buttonStyle(MastTapTargetStyle()).disabled(model.pairingInProgress || model.pairingCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty).accessibilityIdentifier("connect-button")
        if let error = model.pairingError { Text(error).font(.system(size: 13)).foregroundStyle(style.ink(scheme)).accessibilityIdentifier("pairing-error") }
        Picker("Style", selection: $styleChoice) { Text("Match Mac").tag("match"); Text("Default").tag("default"); Text("Industrial").tag("industrial"); Text("Horizon").tag("horizon") }.tint(style.muted(scheme))
        Picker("Appearance", selection: $appearanceChoice) { Text("System").tag("system"); Text("Light").tag("light"); Text("Dark").tag("dark") }.tint(style.muted(scheme))
        HStack { Circle().fill(model.connection.hasPrefix("Connected") ? Color.green : style.slate).frame(width: 7, height: 7); Text(model.connection); Spacer(); Button("Disconnect") { model.forget() } }.font(.system(size: 13)).foregroundStyle(style.muted(scheme))
        if model.isPaired { Button("Forget this Mac") { confirmForget = true }.foregroundStyle(style.muted(scheme)); if !forgetError.isEmpty { Text(forgetError).font(.system(size: 12)) } }
        if snapshot { HStack { Text("Demo mode"); Spacer(); Text("On").foregroundStyle(style.muted(scheme)) }.font(.system(size: 14)); HStack { Text("Show worker chats"); Spacer(); Text("Off").foregroundStyle(style.muted(scheme)) }.font(.system(size: 14)) }
        else { Toggle("Demo mode", isOn: $model.demo).tint(style.muted(scheme)).onChange(of: model.demo) { _, demo in if !demo { model.hasLoaded = false; model.requests = []; model.chats = [] }; Task { await model.refresh() } }.font(.system(size: 14)); Toggle("Show worker chats", isOn: $model.showWorkers).tint(style.muted(scheme)).font(.system(size: 14)) }
        if snapshot { HStack { Text("Reply & resolve"); Spacer(); Text(model.replyEnabled ? "On" : "Off").foregroundStyle(style.muted(scheme)) }.font(.system(size: 14)) } else { Toggle("Reply & resolve", isOn: $model.replyEnabled).tint(style.muted(scheme)).font(.system(size: 14)) }
        if !model.demo { VStack(alignment: .leading, spacing: 12) { Text("SHARED SPACES").font(.system(size: 10, weight: .bold)).tracking(1.3).foregroundStyle(style.muted(scheme)); if model.sharedSpaces.isEmpty { Text("No Spaces shared — run `spaces share` on your Mac").font(.system(size: 13)) }; ForEach(model.sharedSpaces) { space in Toggle(space.name, isOn: Binding(get: { !model.hiddenSpaceIds.contains(space.id) }, set: { enabled in if enabled { model.hiddenSpaceIds.remove(space.id) } else { model.hiddenSpaceIds.insert(space.id) } })).tint(style.muted(scheme)).font(.system(size: 14)) } } }
        if snapshot { HStack { Text("Quick actions"); Spacer(); Text(quickActions ? "On" : "Off").foregroundStyle(style.muted(scheme)) }.font(.system(size: 14)); HStack { Text("Lock after"); Spacer(); Text(lockTimeout == 0 ? "Immediately" : "\(lockTimeout / 60) minutes").foregroundStyle(style.muted(scheme)) }.font(.system(size: 14)) }
        else { Toggle("Quick actions", isOn: $quickActions).tint(style.muted(scheme)).font(.system(size: 14)); Picker("Lock after", selection: $lockTimeout) { Text("Immediately").tag(0); Text("1 minute").tag(60); Text("5 minutes").tag(300); Text("15 minutes").tag(900) }.font(.system(size: 14)) }
    }.font(style.ui(14)).foregroundStyle(style.ink(scheme)).padding(.horizontal, 22).padding(.top, 24) }.background(style.surface(scheme)).toolbar(.hidden, for: .navigationBar).confirmationDialog("Revoke this device and forget this Mac?", isPresented: $confirmForget) { Button("Forget this Mac", role: .destructive) { Task { do { try await model.forgetThisMac(); forgetError = "" } catch { forgetError = "Could not revoke this device. Try again or revoke it on the Mac." } } }; Button("Cancel", role: .cancel) {} }.confirmationDialog("Disconnect before changing the bridge?", isPresented: $confirmChange) { Button("Disconnect and continue", role: .destructive) { model.forget(); if scanAfterChange { scanning = true } }; Button("Cancel", role: .cancel) {} } message: { Text("The saved device token will be cleared.") }.sheet(isPresented: $scanning) { QRScanner { value in scanning = false; if let pair = parsePairingQR(value) { model.address = pair.url; model.pairingCode = pair.code; model.pairingError = nil; scanError = "" } else { scanError = "Invalid pairing code." } } } }
}

func parsePairingQR(_ value: String) -> (url: String, code: String)? {
    guard let parts = URLComponents(string: value), parts.scheme == "mast-companion", parts.host == "pair", parts.path.isEmpty, let items = parts.queryItems, items.count == 2,
          let urlString = items.first(where: { $0.name == "url" })?.value, let code = items.first(where: { $0.name == "code" })?.value, !code.isEmpty, code.count <= 128,
          let url = URLComponents(string: urlString), let host = url.host, url.user == nil, url.password == nil, url.query == nil, url.fragment == nil, (url.path.isEmpty || url.path == "/"), url.scheme == "https" || (url.scheme == "http" && ["localhost", "127.0.0.1"].contains(host)) else { return nil }
    return (urlString, code)
}
struct QRScanner: UIViewControllerRepresentable {
    let found: (String) -> Void
    func makeUIViewController(context: Context) -> ScannerController { let controller = ScannerController(); controller.found = found; return controller }
    func updateUIViewController(_ controller: ScannerController, context: Context) {}
}
final class ScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var found: ((String) -> Void)?
    private let session = AVCaptureSession()
    override func viewDidLoad() {
        super.viewDidLoad(); view.backgroundColor = .black
        guard let camera = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: camera), session.canAddInput(input) else { return }
        session.addInput(input); let output = AVCaptureMetadataOutput(); guard session.canAddOutput(output) else { return }; session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main); output.metadataObjectTypes = [.qr]
        let preview = AVCaptureVideoPreviewLayer(session: session); preview.videoGravity = .resizeAspectFill; preview.frame = view.bounds; view.layer.addSublayer(preview)
        DispatchQueue.global(qos: .userInitiated).async { self.session.startRunning() }
    }
    override func viewDidDisappear(_ animated: Bool) { super.viewDidDisappear(animated); DispatchQueue.global(qos: .userInitiated).async { self.session.stopRunning() } }
    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) { guard let code = (objects.first as? AVMetadataMachineReadableCodeObject)?.stringValue, let callback = found else { return }; found = nil; callback(code) }
}
