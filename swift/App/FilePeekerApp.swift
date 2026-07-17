import SwiftUI

@main
struct FilePeekerApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        .defaultSize(width: 980, height: 640)
        .commands {
            GoCommands()
        }
    }
}

private struct ConnectToServerKey: FocusedValueKey {
    typealias Value = () -> Void
}

extension FocusedValues {
    var connectToServer: (() -> Void)? {
        get { self[ConnectToServerKey.self] }
        set { self[ConnectToServerKey.self] = newValue }
    }
}

private struct GoCommands: Commands {
    @FocusedValue(\.connectToServer) private var connectToServer

    var body: some Commands {
        CommandMenu("Go") {
            Button("Connect to Server") {
                connectToServer?()
            }
            .disabled(connectToServer == nil)
        }
    }
}
