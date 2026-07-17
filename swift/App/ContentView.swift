import SwiftUI

extension DirectoryEntry: Identifiable {
    public var id: String { path }
}

struct ContentView: View {
    @StateObject private var model = BrowserModel()
    @State private var selection: String?
    @State private var viewStyle = ViewStyle.list
    @State private var sortOrder = SortOrder.name
    @State private var searchText = ""
    @State private var isMorePresented = false

    var body: some View {
        NavigationSplitView {
            FinderSidebar()
                .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 280)
        } detail: {
            FinderContent(
                model: model,
                selection: $selection,
                searchText: searchText,
                sortOrder: sortOrder,
                viewStyle: viewStyle
            )
        }
        .navigationSplitViewStyle(.balanced)
        .navigationTitle(toolbarTitle)
        .toolbar {
            ToolbarItemGroup(placement: .navigation) {
                Button("Back", systemImage: "chevron.left") {}
                    .help("Back")
                    .disabled(true)
                Button("Forward", systemImage: "chevron.right") {}
                    .help("Forward")
                    .disabled(true)
            }

            ToolbarItem(id: "view-style", placement: .primaryAction) {
                Picker("View", selection: $viewStyle) {
                    ForEach(ViewStyle.allCases) { style in
                        Image(systemName: style.symbol)
                            .tag(style)
                            .disabled(style == .columns || style == .gallery)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 152)
                .help("View")
            }

            if #available(macOS 26.0, *) {
                ToolbarSpacer(.fixed, placement: .primaryAction)
            }

            ToolbarItem(id: "group-items", placement: .primaryAction) {
                Menu {
                    Picker("Sort By", selection: $sortOrder) {
                        ForEach(SortOrder.allCases) { order in
                            Text(order.title)
                                .tag(order)
                        }
                    }
                } label: {
                    Label("Sort", systemImage: "square.grid.3x3.square")
                }
                .help("Sort Items")
            }

            if #available(macOS 26.0, *) {
                ToolbarSpacer(.fixed, placement: .primaryAction)
            }

            ToolbarItemGroup(placement: .primaryAction) {
                if let selection {
                    ShareLink(item: URL(fileURLWithPath: selection)) {
                        Label("Share", systemImage: "square.and.arrow.up")
                    }
                    .help("Share")
                } else {
                    Button("Share", systemImage: "square.and.arrow.up") {}
                        .help("Share")
                        .disabled(true)
                }

                Button("Tags", systemImage: "tag") {}
                    .help("Edit Tags")
                    .disabled(true)

                Button("More", systemImage: "ellipsis") {
                    isMorePresented.toggle()
                }
                .help("More")
                .popover(isPresented: $isMorePresented, arrowEdge: .bottom) {
                    VStack(alignment: .leading, spacing: 8) {
                        Button("Open") {
                            openSelection()
                            isMorePresented = false
                        }
                        .disabled(selection == nil)

                        Divider()

                        Button("Get Info") {}
                            .disabled(true)
                    }
                    .padding(12)
                    .frame(width: 160)
                }
            }

            if #available(macOS 26.0, *) {
                ToolbarSpacer(.fixed, placement: .primaryAction)
            }
        }
        .searchable(text: $searchText, placement: .toolbar, prompt: "Search")
        .frame(minWidth: 720, minHeight: 460)
        .task {
            model.start()
        }
    }

    private func openSelection() {
        guard let selection,
              let entry = model.entries.first(where: { $0.path == selection }) else {
            return
        }
        model.open(entry)
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
    let searchText: String
    let sortOrder: SortOrder
    let viewStyle: ViewStyle

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
            if viewStyle == .icons {
                iconGrid
            } else {
                fileTable
            }
        }
    }

    private var fileTable: some View {
        Table(visibleEntries, selection: $selection) {
            TableColumn("Name") { entry in
                HStack(spacing: 8) {
                    Image(systemName: symbol(for: entry))
                        .foregroundStyle(entry.kind == .directory ? Color.accentColor : .secondary)
                        .frame(width: 18)
                    Text(entry.name)
                        .lineLimit(1)
                }
                .contentShape(Rectangle())
                .onTapGesture(count: 2) {
                    model.open(entry)
                }
            }

            TableColumn("Kind") { entry in
                Text(kindName(for: entry))
                    .foregroundStyle(.secondary)
            }
            .width(min: 100, ideal: 150)
        }
        .tableStyle(.inset(alternatesRowBackgrounds: true))
    }

    private var iconGrid: some View {
        ScrollView {
            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: 96), spacing: 18)],
                spacing: 20
            ) {
                ForEach(visibleEntries, id: \.path) { entry in
                    VStack(spacing: 8) {
                        Image(systemName: symbol(for: entry))
                            .font(.system(size: 38))
                            .foregroundStyle(entry.kind == .directory ? Color.accentColor : .secondary)
                        Text(entry.name)
                            .font(.caption)
                            .lineLimit(2)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(6)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        selection = entry.path
                    }
                    .onTapGesture(count: 2) {
                        model.open(entry)
                    }
                }
            }
            .padding(18)
        }
    }

    private var visibleEntries: [DirectoryEntry] {
        let filtered = searchText.isEmpty
            ? model.entries
            : model.entries.filter {
                $0.name.localizedCaseInsensitiveContains(searchText)
            }

        return filtered.sorted { lhs, rhs in
            switch sortOrder {
            case .name:
                return lhs.name.localizedStandardCompare(rhs.name) == .orderedAscending
            case .kind:
                let lhsKind = kindName(for: lhs)
                let rhsKind = kindName(for: rhs)
                if lhsKind == rhsKind {
                    return lhs.name.localizedStandardCompare(rhs.name) == .orderedAscending
                }
                return lhsKind.localizedStandardCompare(rhsKind) == .orderedAscending
            }
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
        if searchText.isEmpty {
            return "\(model.entries.count) \(model.entries.count == 1 ? "item" : "items")"
        }
        return "\(visibleEntries.count) of \(model.entries.count) items"
    }

    private func symbol(for entry: DirectoryEntry) -> String {
        switch entry.kind {
        case .directory:
            return "folder.fill"
        case .symlink:
            return "link"
        case .file:
            return "doc"
        case .other:
            return "questionmark.square"
        }
    }

    private func kindName(for entry: DirectoryEntry) -> String {
        switch entry.kind {
        case .directory:
            return "Folder"
        case .symlink:
            return "Alias"
        case .file:
            return "Document"
        case .other:
            return "Other"
        }
    }
}

private enum SortOrder: String, CaseIterable, Identifiable {
    case name
    case kind

    var id: Self { self }

    var title: String {
        switch self {
        case .name:
            return "Name"
        case .kind:
            return "Kind"
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
