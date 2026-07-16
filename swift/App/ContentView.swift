import SwiftUI

struct ContentView: View {
    @StateObject private var model = BrowserModel()
    @State private var selection: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(model.currentPath)
                .font(.headline)
                .lineLimit(1)
                .truncationMode(.middle)

            List(model.entries, id: \.path, selection: $selection) { entry in
                HStack(spacing: 8) {
                    Image(systemName: symbol(for: entry))
                        .frame(width: 18)
                    Text(entry.name)
                    Spacer()
                    if entry.navigable {
                        Image(systemName: "chevron.right")
                            .foregroundStyle(.secondary)
                    }
                }
                .contentShape(Rectangle())
                .onTapGesture(count: 2) {
                    model.open(entry)
                }
            }

            HStack {
                if model.isLoading {
                    ProgressView()
                        .controlSize(.small)
                    Text("Loading…")
                } else if let error = model.errorMessage {
                    Text(error)
                        .foregroundStyle(.red)
                        .lineLimit(2)
                } else {
                    Text("Double-click a directory to open it")
                        .foregroundStyle(.secondary)
                }
            }
            .frame(minHeight: 24)
        }
        .padding(16)
        .frame(minWidth: 620, minHeight: 420)
        .task {
            model.start()
        }
    }

    private func symbol(for entry: DirectoryEntry) -> String {
        switch entry.kind {
        case .directory:
            return "folder"
        case .symlink:
            return "link"
        case .file:
            return "doc"
        case .other:
            return "questionmark.square"
        }
    }
}
