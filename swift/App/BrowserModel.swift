import Combine
import Foundation

@MainActor
final class BrowserModel: ObservableObject {
    @Published private(set) var snapshot: StateSnapshot?
    @Published private(set) var loadingTreePaths: Set<String> = []
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?

    private let client = Client()
    private var session: Session?
    private var state: State?
    private var loadTask: Task<Void, Never>?
    private var expansionTasks: [String: Task<Void, Never>] = [:]
    private var generation: UInt64 = 0
    private var homePath = FileManager.default.homeDirectoryForCurrentUser.path

    var currentPath: String {
        snapshot?.path ?? homePath
    }

    var treeRows: [StateRow] {
        snapshot?.rows ?? []
    }

    var entries: [DirectoryEntry] {
        treeRows.lazy.filter { $0.depth == 0 }.map(\.entry)
    }

    func start() {
        guard session == nil, loadTask == nil else {
            return
        }

        isLoading = true
        errorMessage = nil
        let path = homePath
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
                let state = try await session.openState(path: path)
                try Task.checkCancellation()
                self.session = session
                self.state = state
                snapshot = state.snapshot()
                isLoading = false
                loadTask = nil
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

        guard let session else {
            return
        }

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
              let rowIndex = treeRows.firstIndex(where: { $0.entry.path == entry.path }),
              !loadingTreePaths.contains(entry.path),
              let state else {
            return
        }

        let path = entry.path
        if treeRows[rowIndex].expanded {
            do {
                let snapshot = try state.collapse(path: path)
                cancelTasksMissing(from: snapshot)
                self.snapshot = snapshot
            } catch {
                errorMessage = String(describing: error)
            }
            return
        }

        let requestGeneration = generation
        loadingTreePaths.insert(path)
        expansionTasks[path] = Task {
            do {
                let snapshot = try await state.expand(path: path)
                try Task.checkCancellation()
                guard requestGeneration == generation else {
                    return
                }
                self.snapshot = snapshot
            } catch is CancellationError {
                return
            } catch {
                guard requestGeneration == generation else {
                    return
                }
                snapshot = state.snapshot()
            }
            loadingTreePaths.remove(path)
            expansionTasks[path] = nil
        }
    }

    func connect(to destination: String) async throws {
        let newSession = try await client.connect(
            config: SessionConfig(target: .ssh(destination: destination))
        )
        let remoteRoot = try await newSession.currentRoot()
        let newState = try await newSession.openState(path: remoteRoot)
        try Task.checkCancellation()

        generation &+= 1
        loadTask?.cancel()
        loadTask = nil
        cancelExpansionTasks()
        session = newSession
        state = newState
        homePath = remoteRoot
        snapshot = newState.snapshot()
        isLoading = false
        errorMessage = nil
    }

    private func openDirectory(_ path: String) {
        guard let session else {
            return
        }

        generation &+= 1
        let requestGeneration = generation
        loadTask?.cancel()
        cancelExpansionTasks()
        isLoading = true
        errorMessage = nil

        loadTask = Task {
            do {
                let newState = try await session.openState(path: path)
                try Task.checkCancellation()
                guard requestGeneration == generation else {
                    return
                }
                state = newState
                snapshot = newState.snapshot()
                isLoading = false
                loadTask = nil
            } catch is CancellationError {
                return
            } catch {
                guard requestGeneration == generation else {
                    return
                }
                errorMessage = String(describing: error)
                isLoading = false
                loadTask = nil
            }
        }
    }

    private func cancelTasksMissing(from snapshot: StateSnapshot) {
        let visiblePaths = Set(snapshot.rows.map(\.entry.path))
        let removedTaskPaths = expansionTasks.keys.filter { !visiblePaths.contains($0) }
        for path in removedTaskPaths {
            expansionTasks[path]?.cancel()
            expansionTasks[path] = nil
            loadingTreePaths.remove(path)
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
