import SwiftUI

struct ContentView: View {
    @StateObject private var model = BrowserModel()
    @State private var selection: String?
    @State private var viewStyle = ViewStyle.icons

    var body: some View {
        NavigationSplitView {
            FinderSidebar()
                .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 280)
        } detail: {
            FinderContent(model: model, selection: $selection)
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle("File Peeker")
        .toolbar {
            ToolbarItemGroup(placement: .navigation) {
                Button("Back", systemImage: "chevron.left") {}
                    .help("Back")
                    .disabled(true)
                Button("Forward", systemImage: "chevron.right") {}
                    .help("Forward")
                    .disabled(true)
            }

            ToolbarItem(placement: .principal) {
                Text(toolbarTitle)
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(model.currentPath)
            }

            ToolbarItemGroup(placement: .primaryAction) {
                Picker("View", selection: $viewStyle) {
                    ForEach(ViewStyle.allCases) { style in
                        Image(systemName: style.symbol)
                            .tag(style)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 116)
                .help("View")
                .disabled(true)

                Button("Group", systemImage: "square.grid.3x3.square") {}
                    .help("Group Items")
                    .disabled(true)
                Button("Share", systemImage: "square.and.arrow.up") {}
                    .help("Share")
                    .disabled(true)
                Button("Tags", systemImage: "tag") {}
                    .help("Edit Tags")
                    .disabled(true)
                Button("More", systemImage: "ellipsis.circle") {}
                    .help("More")
                    .disabled(true)

                TextField("Search", text: .constant(""))
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 180)
                    .disabled(true)
            }
        }
        .frame(minWidth: 720, minHeight: 460)
        .task {
            model.start()
        }
    }

    private var toolbarTitle: String {
        guard !model.currentPath.isEmpty else {
            return model.isLoading ? "Loading…" : "File Peeker"
        }

        let name = URL(fileURLWithPath: model.currentPath).lastPathComponent
        return name.isEmpty ? model.currentPath : name
    }
}

private struct FinderContent: View {
    @ObservedObject var model: BrowserModel
    @Binding var selection: String?

    var body: some View {
        VStack(spacing: 0) {
            content

            Divider()

            statusBar
        }
        .onChange(of: model.currentPath) {
            selection = nil
        }
    }

    @ViewBuilder
    private var content: some View {
        if let error = model.errorMessage, model.entries.isEmpty, !model.isLoading {
            ContentUnavailableView(
                "Unable to Open Folder",
                systemImage: "exclamationmark.triangle",
                description: Text(error)
            )
        } else {
            List(model.entries, id: \.path, selection: $selection) { entry in
                HStack(spacing: 8) {
                    Image(systemName: symbol(for: entry))
                        .frame(width: 18)
                    Text(entry.name)
                        .lineLimit(1)
                    Spacer()
                }
                .tag(entry.path)
                .contentShape(Rectangle())
                .onTapGesture(count: 2) {
                    model.open(entry)
                }
            }
            .listStyle(.inset)
        }
    }

    private var statusBar: some View {
        HStack(spacing: 6) {
            if model.isLoading {
                ProgressView()
                    .controlSize(.small)
                Text("Loading…")
                    .foregroundStyle(.secondary)
            } else if let error = model.errorMessage {
                Image(systemName: "exclamationmark.triangle.fill")
                Text(error)
                    .lineLimit(1)
            } else {
                Text(itemCount)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(model.errorMessage == nil ? Color.secondary : Color.red)
        .padding(.horizontal, 12)
        .frame(height: 28)
        .background(.bar)
    }

    private var itemCount: String {
        "\(model.entries.count) \(model.entries.count == 1 ? "item" : "items")"
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

private struct FinderSidebar: View {
    var body: some View {
        List {
            SidebarSection("Favorites", items: SidebarItem.favorites)
            SidebarSection("iCloud", items: SidebarItem.iCloud)
            SidebarSection("Locations", items: SidebarItem.locations)
        }
        .listStyle(.sidebar)
    }
}

private struct SidebarSection: View {
    let title: String
    let items: [SidebarItem]

    init(_ title: String, items: [SidebarItem]) {
        self.title = title
        self.items = items
    }

    var body: some View {
        Section(title) {
            ForEach(items) { item in
                Label(item.title, systemImage: item.symbol)
                    .allowsHitTesting(false)
            }
        }
    }
}

private struct SidebarItem: Identifiable {
    let title: String
    let symbol: String

    var id: String { title }

    static let favorites = [
        SidebarItem(title: "AirDrop", symbol: "airdrop"),
        SidebarItem(title: "Recents", symbol: "clock"),
        SidebarItem(title: "Applications", symbol: "square.grid.2x2"),
        SidebarItem(title: "Desktop", symbol: "desktopcomputer"),
        SidebarItem(title: "Documents", symbol: "doc"),
        SidebarItem(title: "Downloads", symbol: "arrow.down.circle"),
    ]

    static let iCloud = [
        SidebarItem(title: "iCloud Drive", symbol: "icloud"),
        SidebarItem(title: "Shared", symbol: "person.2"),
    ]

    static let locations = [
        SidebarItem(title: "Macintosh HD", symbol: "internaldrive"),
        SidebarItem(title: "Network", symbol: "network"),
    ]
}

private enum ViewStyle: String, CaseIterable, Identifiable {
    case icons
    case list
    case columns
    case gallery

    var id: Self { self }

    var symbol: String {
        switch self {
        case .icons:
            return "square.grid.2x2"
        case .list:
            return "list.bullet"
        case .columns:
            return "rectangle.split.3x1"
        case .gallery:
            return "rectangle.on.rectangle"
        }
    }
}

#Preview {
    ContentView()
        .frame(width: 980, height: 640)
}
