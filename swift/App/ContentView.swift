import SwiftUI

struct ContentView: View {
    var body: some View {
        NavigationSplitView {
            List {
                Label("Home", systemImage: "house.fill")
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 280)
        } detail: {
            ContentUnavailableView("Home", systemImage: "house.fill")
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle("Home")
        .frame(minWidth: 720, minHeight: 460)
    }
}

#Preview {
    ContentView()
        .frame(width: 980, height: 640)
}
