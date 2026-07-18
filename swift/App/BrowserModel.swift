import Combine
import Foundation

struct DisplayRow: Identifiable {
    var entry: DirectoryEntry
    var parentPath: String?
    var depth: UInt32
    var expanded: Bool
    var errorMessage: String?

    var id: String { entry.path }
}

@MainActor
final class BrowserModel: ObservableObject {
    @Published private(set) var currentPath: String
    @Published private(set) var treeRows: [DisplayRow] = []
    @Published private(set) var loadingTreePaths: Set<String> = []
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?

    private let client = Client()
    private var session: Session?
    private var loadTask: Task<Void, Never>?
    private var expansionTasks: [String: Task<Void, Never>] = [:]
    private var generation: UInt64 = 0
    private var homePath = FileManager.default.homeDirectoryForCurrentUser.path

    init() {
        currentPath = FileManager.default.homeDirectoryForCurrentUser.path
    }

    var entries: [DirectoryEntry] {
        treeRows.lazy.filter { $0.depth == 0 }.map(\.entry)
    }

    func start() {
        guard session == nil, loadTask == nil else {
            return
        }

        generation &+= 1
        let requestGeneration = generation
        prepareRoot(path: homePath)
        loadTask = Task {
            do {
                guard let serverURL = Bundle.main.url(
                    forResource: "file-peeker-server",
                    withExtension: nil
                ) else {
                    throw BrowserUIError.missingServer
                }

                let session = try await client.connect(
                    config: SessionConfig(
                        target: .local(serverExecutablePath: serverURL.path)
                    )
                )
                try Task.checkCancellation()
                guard requestGeneration == generation else { return }
                self.session = session
                try await consumeRoot(
                    session: session,
                    path: homePath,
                    requestGeneration: requestGeneration
                )
            } catch is CancellationError {
                return
            } catch {
                finishRootFailure(error, requestGeneration: requestGeneration)
            }
        }
    }

    func open(_ entry: DirectoryEntry) {
        if entry.navigable {
            openDirectory(entry.path)
            return
        }

        guard let session else { return }
        errorMessage = nil
        Task {
            do {
                try await session.open(path: entry.path)
            } catch is CancellationError {
                return
            } catch {
                errorMessage = String(describing: error)
            }
        }
    }

    func openHome() {
        openDirectory(homePath)
    }

    func entry(at path: String) -> DirectoryEntry? {
        treeRows.first(where: { $0.entry.path == path })?.entry
    }

    func toggleExpansion(of entry: DirectoryEntry) {
        guard entry.navigable,
              let index = treeRows.firstIndex(where: { $0.entry.path == entry.path }),
              let session else {
            return
        }

        if treeRows[index].expanded {
            collapse(path: entry.path)
            return
        }

        let path = entry.path
        let requestGeneration = generation
        treeRows[index].expanded = true
        treeRows[index].errorMessage = nil
        loadingTreePaths.insert(path)
        expansionTasks[path] = Task {
            do {
                let listing = try await session.list(path: path)
                while let batch = try await listing.nextBatch() {
                    try Task.checkCancellation()
                    guard requestGeneration == generation,
                          treeRows.contains(where: { $0.entry.path == path && $0.expanded }) else {
                        return
                    }
                    merge(batch: batch, parentPath: path)
                }
                guard requestGeneration == generation else { return }
                finishExpansion(path: path)
            } catch is CancellationError {
                return
            } catch {
                guard requestGeneration == generation else { return }
                loadingTreePaths.remove(path)
                expansionTasks[path] = nil
                if let index = treeRows.firstIndex(where: { $0.entry.path == path }) {
                    treeRows[index].errorMessage = String(describing: error)
                }
            }
        }
    }

    func connect(to destination: String) async throws {
        let newSession = try await client.connect(
            config: SessionConfig(target: .ssh(destination: destination))
        )
        let remoteRoot = try await newSession.currentRoot()
        try Task.checkCancellation()

        session = newSession
        homePath = remoteRoot
        beginRootListing(session: newSession, path: remoteRoot)
    }

    private func openDirectory(_ path: String) {
        guard let session else { return }
        beginRootListing(session: session, path: path)
    }

    private func beginRootListing(session: Session, path: String) {
        generation &+= 1
        let requestGeneration = generation
        loadTask?.cancel()
        cancelExpansionTasks()
        prepareRoot(path: path)

        loadTask = Task {
            do {
                try await consumeRoot(
                    session: session,
                    path: path,
                    requestGeneration: requestGeneration
                )
            } catch is CancellationError {
                return
            } catch {
                finishRootFailure(error, requestGeneration: requestGeneration)
            }
        }
    }

    private func prepareRoot(path: String) {
        currentPath = path
        treeRows = []
        isLoading = true
        errorMessage = nil
    }

    private func consumeRoot(
        session: Session,
        path: String,
        requestGeneration: UInt64
    ) async throws {
        let listing = try await session.list(path: path)
        while let batch = try await listing.nextBatch() {
            try Task.checkCancellation()
            guard requestGeneration == generation else { return }
            merge(batch: batch, parentPath: nil)
        }
        guard requestGeneration == generation else { return }
        isLoading = false
        loadTask = nil
    }

    private func finishRootFailure(_ error: Error, requestGeneration: UInt64) {
        guard requestGeneration == generation else { return }
        errorMessage = String(describing: error)
        isLoading = false
        loadTask = nil
    }

    private func merge(batch: [DirectoryEntry], parentPath: String?) {
        let depth = parentPath
            .flatMap { path in treeRows.first(where: { $0.entry.path == path })?.depth }
            .map { $0 + 1 } ?? 0
        for entry in batch {
            if let index = treeRows.firstIndex(where: { $0.entry.path == entry.path }) {
                treeRows[index].entry = entry
            } else {
                treeRows.append(
                    DisplayRow(
                        entry: entry,
                        parentPath: parentPath,
                        depth: depth,
                        expanded: false,
                        errorMessage: nil
                    )
                )
            }
        }
    }

    private func finishExpansion(path: String) {
        loadingTreePaths.remove(path)
        expansionTasks[path] = nil
    }

    private func collapse(path: String) {
        var removedPaths: Set<String> = []
        var frontier: Set<String> = [path]
        while !frontier.isEmpty {
            let children = Set(
                treeRows
                    .filter { row in
                        guard let parentPath = row.parentPath else { return false }
                        return frontier.contains(parentPath)
                    }
                    .map(\.entry.path)
            )
            removedPaths.formUnion(children)
            frontier = children
        }
        treeRows.removeAll { removedPaths.contains($0.entry.path) }
        if let index = treeRows.firstIndex(where: { $0.entry.path == path }) {
            treeRows[index].expanded = false
            treeRows[index].errorMessage = nil
        }

        let cancelledPaths = expansionTasks.keys.filter {
            $0 == path || removedPaths.contains($0)
        }
        for cancelledPath in cancelledPaths {
            expansionTasks[cancelledPath]?.cancel()
            expansionTasks[cancelledPath] = nil
            loadingTreePaths.remove(cancelledPath)
        }
    }

    private func cancelExpansionTasks() {
        for task in expansionTasks.values {
            task.cancel()
        }
        expansionTasks.removeAll()
        loadingTreePaths.removeAll()
    }
}

private enum BrowserUIError: LocalizedError {
    case missingServer

    var errorDescription: String? {
        "The bundled file-peeker-server executable is missing."
    }
}
