import Combine
import Foundation

@MainActor
final class BrowserModel: ObservableObject {
    @Published private(set) var currentPath =
        FileManager.default.homeDirectoryForCurrentUser.path
    @Published private(set) var entries: [DirectoryEntry] = []
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?

    private var client: BrowserClient?
    private var loadTask: Task<Void, Never>?
    private var generation: UInt64 = 0

    func start() {
        guard client == nil, loadTask == nil else {
            return
        }

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
                self.client = client
                openDirectory(currentPath)
            } catch {
                errorMessage = String(describing: error)
                loadTask = nil
            }
        }
    }

    func open(_ entry: DirectoryEntry) {
        guard entry.navigable else {
            return
        }
        openDirectory(entry.path)
    }

    private func openDirectory(_ path: String) {
        guard let client else {
            return
        }

        generation &+= 1
        let requestGeneration = generation
        loadTask?.cancel()
        currentPath = path
        entries = []
        isLoading = true
        errorMessage = nil

        loadTask = Task {
            do {
                let listing = try await client.startListing(path: path)
                while !Task.isCancelled, let entry = try await listing.nextEntry() {
                    guard requestGeneration == generation else {
                        return
                    }
                    entries.append(entry)
                }
                guard requestGeneration == generation else {
                    return
                }
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
}

private enum BrowserUIError: LocalizedError {
    case missingServer

    var errorDescription: String? {
        "The bundled file-peeker-server executable is missing."
    }
}
