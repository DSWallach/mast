import Foundation
import LocalAuthentication
import Combine

func requiresConfirmation(quickActions: Bool) -> Bool { !quickActions }

enum AuthenticationResult: Equatable { case success, failure, noPasscode }
protocol DeviceAuthenticator { func authenticate(completion: @escaping (AuthenticationResult) -> Void) }
struct LocalDeviceAuthenticator: DeviceAuthenticator {
    func authenticate(completion: @escaping (AuthenticationResult) -> Void) {
        let context = LAContext(); var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else { completion(error?.code == LAError.passcodeNotSet.rawValue ? .noPasscode : .failure); return }
        context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "Unlock Mast") { success, _ in DispatchQueue.main.async { completion(success ? .success : .failure) } }
    }
}
@MainActor
final class AppLock: ObservableObject {
    @Published private(set) var locked = true
    @Published private(set) var obscured = true
    @Published private(set) var authenticating = false
    @Published private(set) var noPasscode = false
    private let authenticator: any DeviceAuthenticator
    private let now: () -> Date
    private var backgroundedAt: Date?
    private var active = false
    private var generation = 0
    init(authenticator: any DeviceAuthenticator = LocalDeviceAuthenticator(), now: @escaping () -> Date = Date.init) { self.authenticator = authenticator; self.now = now }
    func inactive() { obscured = true }
    func background() { active = false; generation += 1; authenticating = false; obscured = true; backgroundedAt = now() }
    func foreground(timeout: TimeInterval) {
        active = true
        if locked || backgroundedAt.map({ now().timeIntervalSince($0) >= timeout }) == true { locked = true; obscured = true; unlock() }
        else { obscured = false }
        backgroundedAt = nil
    }
    func unlock() {
        guard active && !authenticating else { return }
        authenticating = true; noPasscode = false; let attempt = generation
        authenticator.authenticate { [weak self] result in
            Task { @MainActor in
                guard let self, self.generation == attempt, self.active else { return }
                self.authenticating = false; self.noPasscode = result == .noPasscode
                self.locked = result != .success; self.obscured = result != .success
            }
        }
    }
    func continueWithoutPasscode() { guard active && noPasscode && !authenticating else { return }; locked = false; obscured = false; noPasscode = false }
}
