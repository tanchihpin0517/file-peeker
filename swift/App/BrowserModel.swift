import Combine
import Foundation

struct BrowserRow: Identifiable {
    let id: UInt64
    let entry: DirectoryEntry
}

@MainActor
final class BrowserModel: ObservableObject {
    @Published private(set) var homePath = ""
    @Published private(set) var rows: [BrowserRow] = []
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?

    private let client = Client()
    private var sessionID: String?
    private var session: Session?
    private var loadTask: Task<Void, Never>?
    private var generation: UInt64 = 0
    private var nextRowID: UInt64 = 0

    func start() {
        guard session == nil, loadTask == nil else { return }
        isLoading = true
        errorMessage = nil
        rows = []
        nextRowID = 0
        generation &+= 1
        let requestGeneration = generation

        loadTask = Task {
            var startedID: String?
            do {
                let id = try await client.startSession(target: .local)
                startedID = id
                try Task.checkCancellation()
                guard requestGeneration == generation,
                      let newSession = await client.getSession(id: id) else {
                    try? await client.closeSession(id: id)
                    return
                }

                sessionID = id
                session = newSession

                let path = try await newSession.opResolvePathUniffi(path: "~")
                try Task.checkCancellation()
                guard requestGeneration == generation else { return }
                homePath = path

                let listing = try await newSession.opListDirUniffi(path: path)
                while let entry = try await listing.nextEntry() {
                    try Task.checkCancellation()
                    guard requestGeneration == generation else { return }
                    append(entry)
                }

                guard requestGeneration == generation else { return }
                isLoading = false
                loadTask = nil
            } catch is CancellationError {
                if let startedID, sessionID != startedID {
                    try? await client.closeSession(id: startedID)
                }
            } catch {
                if let startedID, sessionID != startedID {
                    try? await client.closeSession(id: startedID)
                }
                guard requestGeneration == generation else { return }
                errorMessage = String(describing: error)
                isLoading = false
                loadTask = nil
            }
        }
    }

    func shutdown() {
        generation &+= 1
        loadTask?.cancel()
        loadTask = nil
        let closingID = sessionID
        sessionID = nil
        session = nil
        isLoading = false
        guard let closingID else { return }
        Task {
            try? await client.closeSession(id: closingID)
        }
    }

    private func append(_ entry: DirectoryEntry) {
        rows.append(BrowserRow(id: nextRowID, entry: entry))
        nextRowID &+= 1
    }
}
