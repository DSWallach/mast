import SwiftUI

enum MastStyleID: String, CaseIterable { case `default`, industrial, horizon }
struct MastStyle {
    var id: MastStyleID = .default
    var lightTheme: String? = nil, darkTheme: String? = nil
    private func palette(_ scheme: ColorScheme) -> MastStyleID {
        let theme = scheme == .dark ? darkTheme : lightTheme
        if let theme { if theme.hasPrefix("industrial-") { return .industrial }; if theme.hasPrefix("horizon-") { return .horizon }; if theme.hasPrefix("mast-") { return .default } }
        return id
    }
    var grid: CGFloat { id == .default ? 4 : 8 }
    var rowHeight: CGFloat { id == .default ? 29 : id == .industrial ? 32 : 36 }
    var controlSize: CGFloat { id == .default ? 28 : id == .industrial ? 32 : 36 }
    var controlRadius: CGFloat { id == .industrial ? 4 : 12 }
    var panelRadius: CGFloat { id == .industrial ? 0 : 14 }
    var chipRadius: CGFloat { id == .industrial ? 4 : 12 }
    var bubbleRadius: CGFloat { id == .industrial ? 0 : 14 }
    let labelSize: CGFloat = 10, secondarySize: CGFloat = 12, bodySize: CGFloat = 13, titleSize: CGFloat = 20
    var replySize: CGFloat { id == .horizon ? 17.5 : 15 }
    func ui(_ size: CGFloat) -> Font { id == .default ? .system(size: size) : .custom("Geist-Regular", size: size) }
    func semibold(_ size: CGFloat) -> Font { id == .default ? .system(size: size, weight: .semibold) : .custom("Geist-SemiBold", size: size) }
    func serif(_ size: CGFloat) -> Font { id == .horizon ? .custom("EBGaramond-Regular", size: size) : ui(size) }
    func providerSymbol(_ chat: Chat) -> String { (chat.harness ?? "").lowercased().contains("claude") ? "sun.max.fill" : "sparkle" }
    func identity(_ chat: Chat) -> Color {
        guard let model = chat.model, !model.trimmingCharacters(in: .whitespaces).isEmpty else { return slate }
        let harness = (chat.harness ?? "").lowercased()
        if harness.contains("claude") { return Color(red: 1, green: 184/255, blue: 0) }
        if harness.contains("codex") { return Color(red: 29/255, green: 203/255, blue: 67/255) }
        return Color(red: 11/255, green: 167/255, blue: 1)
    }
    var slate: Color { Color(red: 142/255, green: 164/255, blue: 184/255) }
    private func rgb(_ value: UInt) -> Color { Color(red: Double((value >> 16) & 255)/255, green: Double((value >> 8) & 255)/255, blue: Double(value & 255)/255) }
    func surface(_ scheme: ColorScheme) -> Color {
        switch palette(scheme) { case .default: return rgb(scheme == .dark ? 0x060606 : 0xffffff); case .industrial: return rgb(scheme == .dark ? 0x242622 : 0xf5f3ed); case .horizon: return rgb(scheme == .dark ? 0x18191b : 0xffffff) }
    }
    func ink(_ scheme: ColorScheme) -> Color {
        switch palette(scheme) { case .default: return rgb(scheme == .dark ? 0xe8e8ea : 0x303035); case .industrial: return rgb(scheme == .dark ? 0xf5f3ed : 0x272a27); case .horizon: return rgb(scheme == .dark ? 0xffffff : 0x111111) }
    }
    func muted(_ scheme: ColorScheme) -> Color {
        switch palette(scheme) { case .default: return rgb(scheme == .dark ? 0xa9a9ae : 0x62626a); case .industrial: return rgb(scheme == .dark ? 0xbcbdb6 : 0x666862); case .horizon: return scheme == .dark ? slate : rgb(0x54697b) }
    }
    func rule(_ scheme: ColorScheme) -> Color { id == .industrial ? rgb(scheme == .dark ? 0x4a4c46 : 0xaaa9a2) : muted(scheme).opacity(0.18) }
    func action(_ scheme: ColorScheme) -> Color { id == .industrial ? rgb(0xfe4b25) : id == .default ? rgb(scheme == .dark ? 0x8b7cf6 : 0x5b43e8) : muted(scheme) }
    func composer(_ chat: Chat, _ scheme: ColorScheme) -> Color {
        switch id { case .horizon: return identity(chat); case .industrial: return rgb(scheme == .dark ? 0x3a3c37 : 0xefeee8); case .default: return rgb(scheme == .dark ? 0x343438 : 0xededf0) }
    }
    func replyColor(_ chat: Chat, _ scheme: ColorScheme) -> Color { id == .horizon ? provider(chat, scheme) : ink(scheme) }
    func provider(_ chat: Chat, _ scheme: ColorScheme) -> Color {
        guard let model = chat.model, !model.trimmingCharacters(in: .whitespaces).isEmpty else { return muted(scheme) }
        let harness = (chat.harness ?? "").lowercased()
        if palette(scheme) != .horizon {
            if harness.contains("claude") { return rgb(scheme == .dark ? 0xfacc15 : 0xb45309) }
            if harness.contains("codex") { return rgb(scheme == .dark ? 0x4ade80 : 0x16a34a) }
            return rgb(scheme == .dark ? 0x60a5fa : 0x2563eb)
        }
        if harness.contains("claude") { return scheme == .dark ? Color(red: 1, green: 184/255, blue: 0) : Color(red: 140/255, green: 96/255, blue: 0) }
        if harness.contains("codex") { return scheme == .dark ? Color(red: 29/255, green: 203/255, blue: 67/255) : Color(red: 21/255, green: 113/255, blue: 46/255) }
        return scheme == .dark ? Color(red: 11/255, green: 167/255, blue: 1) : Color(red: 0, green: 103/255, blue: 160/255)
    }
    func relative(_ value: Double?) -> String { guard let value, value > 0 else { return "" }; return Date(timeIntervalSince1970: value / 1000).formatted(.relative(presentation: .named)) }
    func relativeISO(_ value: String?) -> String { guard let value else { return "" }; let parser = ISO8601DateFormatter(); parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]; return (parser.date(from: value) ?? ISO8601DateFormatter().date(from: value))?.formatted(.relative(presentation: .named)) ?? "" }
}
private struct MastStyleKey: EnvironmentKey { static let defaultValue = MastStyle() }
extension EnvironmentValues { var mastStyle: MastStyle { get { self[MastStyleKey.self] } set { self[MastStyleKey.self] = newValue } } }
