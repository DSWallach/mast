import SwiftUI

@main
struct MastApp: App {
    var body: some Scene {
        WindowGroup { ContentView() }
    }
}

private struct ActionRequest: Identifiable {
    let id: String
    let space: String
    let kind: String
    let title: String
    let detail: String
    let options: [String]
    let facts: [String]
    let links: [String]
    let origin: String
}

private struct Chat: Identifiable {
    let id: String
    let title: String
    let space: String
    let status: String
    let transcript: [(String, String)]
}

private enum Demo {
    static let requests = [ActionRequest(id: "1", space: "Mast", kind: "Review", title: "Review the companion layout", detail: "Check the inbox and transcript before the next release.", options: ["Looks good", "Needs changes"], facts: ["Priority: Normal", "Screen: Needs you"], links: ["https://example.com/review"], origin: "Companion design chat")]
    static let chats = [Chat(id: "1", title: "Companion design", space: "Mast", status: "Working", transcript: [("You", "Prepare a compact inbox for the phone."), ("Assistant", "The inbox is ready for review.")]), Chat(id: "2", title: "Release notes", space: "Mast", status: "Waiting for input", transcript: [("Assistant", "Which changes should appear in the release notes?")])]
}

private struct ContentView: View {
    var body: some View {
        TabView {
            NavigationStack {
                List {
                    Section { Label("Demo mode · fixture data", systemImage: "eye") .foregroundStyle(.secondary) }
                    Section("Needs you") {
                        ForEach(Demo.requests) { request in
                            NavigationLink {
                                RequestDetail(request: request)
                            } label: {
                                VStack(alignment: .leading, spacing: 5) {
                                    Text(request.title).font(.headline)
                                    Text("\(request.space) · \(request.kind)").font(.subheadline).foregroundStyle(.secondary)
                                }.padding(.vertical, 3)
                            }
                        }
                    }
                }.navigationTitle("Needs you")
            }.tabItem { Label("Needs you", systemImage: "tray") }
            NavigationStack {
                List(Demo.chats) { chat in
                    NavigationLink { ChatDetail(chat: chat) } label: {
                        VStack(alignment: .leading, spacing: 5) {
                            Text(chat.title).font(.headline)
                            Text("\(chat.space) · \(chat.status)").font(.subheadline).foregroundStyle(.secondary)
                        }.padding(.vertical, 3)
                    }
                }.navigationTitle("Chats")
            }.tabItem { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
            NavigationStack {
                Form {
                    Section("Connection") {
                        LabeledContent("Mode", value: "Demo")
                        Text("Bridge connection and pairing are not available yet.").foregroundStyle(.secondary)
                    }
                    Section("Actions") {
                        Toggle("Reply & resolve", isOn: .constant(false)).disabled(true)
                        Text("Replies are unavailable until delivery and resolution are verified.").foregroundStyle(.secondary)
                    }
                }.navigationTitle("Settings")
            }.tabItem { Label("Settings", systemImage: "gearshape") }
        }.tint(Color(red: 0.16, green: 0.37, blue: 0.35))
    }
}

private struct RequestDetail: View {
    let request: ActionRequest
    var body: some View {
        List {
            Section { Text(request.detail) }
            Section("Options") { ForEach(request.options, id: \.self) { Text($0) } }
            Section("Facts") { ForEach(request.facts, id: \.self) { Text($0) } }
            Section("Links") { ForEach(request.links, id: \.self) { Text($0).textSelection(.enabled) } }
            Section("Origin") { Text(request.origin) }
        }.navigationTitle(request.title).navigationBarTitleDisplayMode(.inline)
    }
}

private struct ChatDetail: View {
    let chat: Chat
    @State private var text = ""
    @State private var notice = ""
    var body: some View {
        VStack {
            List {
                Section(chat.status) {
                    ForEach(Array(chat.transcript.enumerated()), id: \.offset) { _, message in
                        VStack(alignment: .leading, spacing: 5) {
                            Text(message.0).font(.caption).foregroundStyle(.secondary)
                            Text(message.1).textSelection(.enabled)
                        }.padding(.vertical, 4)
                    }
                }
            }
            if !notice.isEmpty { Text(notice).font(.caption).foregroundStyle(.secondary) }
            HStack {
                TextField("Message", text: $text, axis: .vertical).textFieldStyle(.roundedBorder)
                Button("Send") { notice = "Demo only: message was not sent."; text = "" }.disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }.padding()
        }.navigationTitle(chat.title).navigationBarTitleDisplayMode(.inline)
    }
}
