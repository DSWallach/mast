import SwiftUI

struct Horizon {
    static let slate = Color(red: 142/255, green: 164/255, blue: 184/255)
    static func surface(_ scheme: ColorScheme) -> Color { scheme == .dark ? Color(red: 24/255, green: 25/255, blue: 27/255) : .white }
    static func ink(_ scheme: ColorScheme) -> Color { scheme == .dark ? .white : Color(red: 17/255, green: 17/255, blue: 17/255) }
    static func muted(_ scheme: ColorScheme) -> Color { scheme == .dark ? slate : Color(red: 84/255, green: 105/255, blue: 123/255) }
    static func provider(_ chat: Chat, _ scheme: ColorScheme) -> Color {
        guard let model = chat.model, !model.trimmingCharacters(in: .whitespaces).isEmpty else { return muted(scheme) }
        let harness = (chat.harness ?? "").lowercased()
        if harness.contains("claude") { return scheme == .dark ? Color(red: 1, green: 184/255, blue: 0) : Color(red: 140/255, green: 96/255, blue: 0) }
        if harness.contains("codex") { return scheme == .dark ? Color(red: 29/255, green: 203/255, blue: 67/255) : Color(red: 21/255, green: 113/255, blue: 46/255) }
        return scheme == .dark ? Color(red: 11/255, green: 167/255, blue: 1) : Color(red: 0, green: 103/255, blue: 160/255)
    }
    static func relative(_ value: Double?) -> String {
        guard let value, value > 0 else { return "" }
        return Date(timeIntervalSince1970: value / 1000).formatted(.relative(presentation: .named))
    }
    static func relativeISO(_ value: String?) -> String {
        guard let value else { return "" }
        let parser = ISO8601DateFormatter(); parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = parser.date(from: value) ?? ISO8601DateFormatter().date(from: value)
        return date?.formatted(.relative(presentation: .named)) ?? ""
    }
}
