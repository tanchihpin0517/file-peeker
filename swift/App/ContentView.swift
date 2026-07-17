import SwiftUI

extension DirectoryEntry: Identifiable {
    public var id: String { path }
}

extension DirectoryTreeRow: Identifiable {
    public var id: String { entry.path }
}

struct ContentView: View {
    @StateObject private var model = BrowserModel()
    @State private var selection: String?
    @State private var viewStyle = ViewStyle.list
    @State private var sortOrder = SortOrder.name
    @State private var searchText = ""
    @State private var isMorePresented = false
    @State private var isConnectToServerPresented = false

    var body: some View {
        NavigationSplitView {
            FinderSidebar(model: model)
                .navigationSplitViewColumnWidth(min: 180, ideal: 220, max: 280)
        } detail: {
            FinderContent(
                model: model,
                selection: $selection,
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

        }
        .searchable(text: $searchText, placement: .toolbar, prompt: "Search")
        .frame(minWidth: 720, minHeight: 460)
        .focusedSceneValue(\.connectToServer) {
            isConnectToServerPresented = true
        }
        .sheet(isPresented: $isConnectToServerPresented) {
            ConnectToServerView(model: model)
        }
        .task {
            model.start()
        }
    }

    private func openSelection() {
        guard let selection,
              let entry = model.entry(at: selection) else {
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

private struct ConnectToServerView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: BrowserModel
    @State private var destination = ""
    @State private var errorMessage: String?
    @State private var isConnecting = false
    @State private var connectionTask: Task<Void, Never>?
    @FocusState private var isDestinationFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Connect to Server")
                .font(.title2)
                .fontWeight(.semibold)

            Text("Enter an SSH destination, such as a host alias from your SSH configuration.")
                .foregroundStyle(.secondary)

            TextField("SSH Destination", text: $destination)
                .focused($isDestinationFocused)
                .disabled(isConnecting)
                .onSubmit(connect)

            if isConnecting {
                ProgressView("Connecting…")
                    .controlSize(.small)
            }

            if let errorMessage {
                Text(errorMessage)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }

            HStack {
                Spacer()

                Button("Cancel", role: .cancel) {
                    connectionTask?.cancel()
                    connectionTask = nil
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button("Connect", action: connect)
                    .keyboardShortcut(.defaultAction)
                    .disabled(trimmedDestination.isEmpty || isConnecting)
            }
        }
        .padding(20)
        .frame(width: 440)
        .onAppear {
            isDestinationFocused = true
        }
        .onDisappear {
            connectionTask?.cancel()
        }
    }

    private var trimmedDestination: String {
        destination.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func connect() {
        let destination = trimmedDestination
        guard !destination.isEmpty, !isConnecting else {
            return
        }

        isConnecting = true
        errorMessage = nil
        connectionTask = Task { @MainActor in
            do {
                try await model.connect(to: destination)
                guard !Task.isCancelled else {
                    return
                }
                connectionTask = nil
                dismiss()
            } catch is CancellationError {
                return
            } catch {
                errorMessage = String(describing: error)
                isConnecting = false
                connectionTask = nil
            }
        }
    }
}

private struct FinderContent: View {
    @ObservedObject var model: BrowserModel
    @Binding var selection: String?
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
        Table(visibleTreeRows, selection: $selection) {
            TableColumn("Name") { row in
                HStack(spacing: 8) {
                    Color.clear
                        .frame(width: CGFloat(row.depth) * 16, height: 1)

                    if row.entry.navigable {
                        if model.loadingTreePaths.contains(row.entry.path) {
                            ProgressView()
                                .controlSize(.mini)
                                .frame(width: 12, height: 12)
                        } else {
                            Button {
                                model.toggleExpansion(of: row.entry)
                            } label: {
                                Image(systemName: row.expanded ? "chevron.down" : "chevron.right")
                                    .font(.caption2)
                                    .frame(width: 12, height: 12)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .help(row.expanded ? "Collapse" : "Expand")
                        }
                    } else {
                        Color.clear
                            .frame(width: 12, height: 12)
                    }

                    Image(systemName: symbol(for: row.entry))
                        .foregroundStyle(row.entry.kind == .directory ? Color.accentColor : .secondary)
                        .frame(width: 18)
                    Text(row.entry.name)
                        .lineLimit(1)

                    if let errorMessage = row.errorMessage {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.red)
                            .help(errorMessage)
                    }
                }
                .frame(height: 10)
            }

            TableColumn("Kind") { row in
                Text(kindName(for: row.entry))
                    .foregroundStyle(.secondary)
                    .frame(height: 10)
            }
            .width(min: 100, ideal: 150)
        }
        .tableStyle(.inset(alternatesRowBackgrounds: true))
        .environment(\.defaultMinListRowHeight, 10)
        .contextMenu(forSelectionType: String.self) { paths in
            tableContextMenu(for: paths)
        } primaryAction: { paths in
            openTableSelection(paths)
        }
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
                    .contextMenu {
                        entryContextMenu(for: entry)
                    }
                }
            }
            .padding(18)
        }
    }

    private var visibleEntries: [DirectoryEntry] {
        model.entries.sorted(by: entryComesBefore)
    }

    private var visibleTreeRows: [DirectoryTreeRow] {
        let rowsByParent = Dictionary(grouping: model.treeRows, by: \.parentPath)
        var result: [DirectoryTreeRow] = []
        appendRows(parentPath: nil, from: rowsByParent, to: &result)
        return result
    }

    private func appendRows(
        parentPath: String?,
        from rowsByParent: [String?: [DirectoryTreeRow]],
        to result: inout [DirectoryTreeRow]
    ) {
        let rows = (rowsByParent[parentPath] ?? []).sorted {
            entryComesBefore($0.entry, $1.entry)
        }
        for row in rows {
            result.append(row)
            if row.expanded {
                appendRows(parentPath: row.entry.path, from: rowsByParent, to: &result)
            }
        }
    }

    private func entryComesBefore(_ lhs: DirectoryEntry, _ rhs: DirectoryEntry) -> Bool {
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

    @ViewBuilder
    private func entryContextMenu(for entry: DirectoryEntry) -> some View {
        Button("Open") {
            selection = entry.path
            model.open(entry)
        }
    }

    @ViewBuilder
    private func tableContextMenu(for paths: Set<String>) -> some View {
        if let entry = entry(in: paths) {
            Button("Open") {
                selection = entry.path
                model.open(entry)
            }
        }
    }

    private func openTableSelection(_ paths: Set<String>) {
        guard let entry = entry(in: paths) else {
            return
        }
        selection = entry.path
        model.open(entry)
    }

    private func entry(in paths: Set<String>) -> DirectoryEntry? {
        guard let path = paths.first else {
            return nil
        }
        return model.entry(at: path)
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
    @ObservedObject var model: BrowserModel

    var body: some View {
        List {
            Button {
                model.openHome()
            } label: {
                Label("Home", systemImage: "house.fill")
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        .listStyle(.sidebar)
    }
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
