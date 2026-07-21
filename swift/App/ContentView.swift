import SwiftUI

struct ContentView: View {
    @StateObject private var model = BrowserModel()

    var body: some View {
        NavigationSplitView {
            List {
                Label("Home", systemImage: "house.fill")
            }
            .listStyle(.sidebar)
            .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 280)
        } detail: {
            VStack(spacing: 0) {
                content
                Divider()
                status
            }
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle("Home")
        .frame(minWidth: 720, minHeight: 460)
        .onAppear { model.start() }
        .onDisappear { model.shutdown() }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading && model.rows.isEmpty {
            ProgressView(model.homePath.isEmpty ? "Connecting…" : "Loading…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = model.errorMessage, model.rows.isEmpty {
            ContentUnavailableView(
                "Unable to List Home",
                systemImage: "exclamationmark.triangle",
                description: Text(error)
            )
        } else if model.rows.isEmpty {
            ContentUnavailableView("Home Is Empty", systemImage: "folder")
        } else {
            List(model.rows) { row in
                Label(row.entry.name, systemImage: symbol(for: row.entry))
            }
        }
    }

    private var status: some View {
        HStack {
            if model.isLoading { ProgressView().controlSize(.small) }
            Text(model.errorMessage ?? "\(model.rows.count) items")
                .foregroundStyle(model.errorMessage == nil ? Color.secondary : Color.red)
                .lineLimit(1)
            Spacer()
            Text(model.homePath).foregroundStyle(.secondary).lineLimit(1)
        }
        .font(.caption)
        .padding(.horizontal, 10)
        .frame(height: 28)
    }

    private func symbol(for entry: DirectoryEntry) -> String {
        switch entry.kind {
        case .directory: "folder.fill"
        case .symlink: "link"
        case .file: "doc"
        case .other: "questionmark.square.dashed"
        }
    }
}

#Preview {
    ContentView()
        .frame(width: 980, height: 640)
}
