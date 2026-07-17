import Combine
import Foundation

@MainActor
final class BrowserModel: ObservableObject {
    @Published private(set) var currentPath =
        FileManager.default.homeDirectoryForCurrentUser.path
    @Published private(set) var treeRows: [DirectoryTreeRow] = []
    @Published private(set) var loadingTreePaths: Set<String> = []
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?

    private var client: BrowserClient?
    private var loadTask: Task<Void, Never>?
    private var expansionTasks: [String: Task<Void, Never>] = [:]
    private var generation: UInt64 = 0
    private var homePath = FileManager.default.homeDirectoryForCurrentUser.path

    var entries: [DirectoryEntry] {
        treeRows.lazy.filter { $0.depth == 0 }.map(\.entry)
    }

    func start() {
        guard client == nil, loadTask == nil else {
            return
        }

        isLoading = true
        errorMessage = nil

        loadTask = Task {
            do {
                guard let serverURL = Bundle.main.url(
                    forResource: "file-peeker-server",
                    withExtension: nil
                ) else {
                    throw BrowserUIError.missingServer
                }

                let client = try await BrowserClient.start(
                    config: ClientConfig(
                        target: .local(serverExecutablePath: serverURL.path)
                    )
                )
                guard !Task.isCancelled else {
                    return
                }
                self.client = client
                loadTask = nil
                openDirectory(currentPath)
            } catch is CancellationError {
                return
            } catch {
                errorMessage = String(describing: error)
                isLoading = false
                loadTask = nil
            }
        }
    }

    func open(_ entry: DirectoryEntry) {
        if entry.navigable {
            openDirectory(entry.path)
            return
        }

        guard let client else {
            return
        }

        errorMessage = nil
        Task {
            do {
                try await client.open(path: entry.path)
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
              let rowIndex = treeRows.firstIndex(where: { $0.entry.path == entry.path }),
              !loadingTreePaths.contains(entry.path) else {
            return
        }

        guard let client else {
            return
        }

        let path = entry.path
        if treeRows[rowIndex].expanded {
            do {
                let rows = try client.collapseTree(path: path)
                let visiblePaths = Set(rows.map(\.entry.path))
                let removedTaskPaths = expansionTasks.keys.filter { !visiblePaths.contains($0) }
                for taskPath in removedTaskPaths {
                    expansionTasks[taskPath]?.cancel()
                    expansionTasks[taskPath] = nil
                    loadingTreePaths.remove(taskPath)
                }
                treeRows = rows
            } catch {
                errorMessage = String(describing: error)
            }
            return
        }

        let requestGeneration = generation
        loadingTreePaths.insert(path)
        expansionTasks[path] = Task {
            do {
                let rows = try await client.expandTree(path: path)
                try Task.checkCancellation()
                guard requestGeneration == generation else {
                    return
                }
                treeRows = rows
            } catch is CancellationError {
                return
            } catch {
                guard requestGeneration == generation else {
                    return
                }
                treeRows = client.treeRows()
            }
            loadingTreePaths.remove(path)
            expansionTasks[path] = nil
        }
    }

    func connect(to destination: String) async throws {
        let remoteClient = try await BrowserClient.start(
            config: ClientConfig(target: .ssh(destination: destination))
        )

        do {
            let remoteRoot = try await remoteClient.currentRoot()
            try Task.checkCancellation()

            generation &+= 1
            loadTask?.cancel()
            loadTask = nil
            client = remoteClient
            homePath = remoteRoot
            openDirectory(remoteRoot)
        } catch {
            try? await remoteClient.close()
            throw error
        }
    }

    private func openDirectory(_ path: String) {
        guard let client else {
            return
        }

        generation &+= 1
        let requestGeneration = generation
        loadTask?.cancel()
        cancelExpansionTasks()
        currentPath = path
        treeRows = []
        isLoading = true
        errorMessage = nil

        loadTask = Task {
            do {
                let rows = try await client.loadTree(path: path)
                try Task.checkCancellation()
                guard requestGeneration == generation else {
                    return
                }
                treeRows = rows
                isLoading = false
            } catch is CancellationError {
                return
            } catch {
                guard requestGeneration == generation else {
                    return
                }
                errorMessage = String(describing: error)
                isLoading = false
            }
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
