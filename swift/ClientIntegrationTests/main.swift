import Darwin
import Foundation

@main
struct ClientIntegrationTests {
    static func main() async {
        testGeneratedValueTypes()
        await testAsyncTypedErrorRoundTrip()
        await testRealServerStartupAndListing()
        print("File Peeker client integration tests passed")
    }

    private static func testGeneratedValueTypes() {
        let localConfig = SessionConfig(
            target: .local(serverExecutablePath: "/tmp/file-peeker-server")
        )
        require(
            localConfig.target
                == .local(serverExecutablePath: "/tmp/file-peeker-server"),
            "SessionConfig did not preserve its local target"
        )

        let sshConfig = SessionConfig(target: .ssh(destination: "example-host"))
        require(
            sshConfig.target == .ssh(destination: "example-host"),
            "SessionConfig did not preserve its SSH target"
        )

        let entry = DirectoryEntry(
            path: "/tmp/example/docs",
            name: "docs",
            kind: .directory,
            navigable: true
        )
        require(entry.name == "docs", "DirectoryEntry did not preserve name")
        require(entry.kind == .directory, "DirectoryEntry did not preserve kind")
        require(entry.navigable, "DirectoryEntry did not preserve navigable")

        let row = StateRow(
            entry: entry,
            parentPath: "/tmp/example",
            depth: 1,
            expanded: false,
            errorMessage: nil
        )
        require(row.entry == entry, "StateRow did not preserve its entry")
        require(row.depth == 1, "StateRow did not preserve depth")

        let snapshot = StateSnapshot(path: "/tmp/example", rows: [row])
        require(snapshot.path == "/tmp/example", "StateSnapshot did not preserve its path")
        require(snapshot.rows == [row], "StateSnapshot did not preserve its rows")

        let metadata = FileMetadata(
            path: "/tmp/example/docs",
            kind: .directory,
            size: 96,
            readonly: false,
            modified: nil
        )
        require(metadata.modified == nil, "FileMetadata did not preserve nil modified")

        print("PASS generated value types")
    }

    private static func testAsyncTypedErrorRoundTrip() async {
        do {
            _ = try await Client().connect(
                config: SessionConfig(
                    target: .local(
                        serverExecutablePath: "/definitely/missing/file-peeker-server"
                    )
                )
            )
            fail("Client.connect unexpectedly succeeded")
        } catch FilePeekerError.ServerStart(let message) {
            require(
                message.contains("cannot launch"),
                "received ServerStart with an unexpected message"
            )
            print("PASS async typed error round trip")
        } catch {
            fail("received an unexpected error: \(error)")
        }
    }

    private static func testRealServerStartupAndListing() async {
        let serverPath =
            ProcessInfo.processInfo.environment["FILE_PEEKER_TEST_SERVER"]
            ?? URL(fileURLWithPath: CommandLine.arguments[0])
                .deletingLastPathComponent()
                .deletingLastPathComponent()
                .appendingPathComponent("target/release/file-peeker-server")
                .path

        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            "file-peeker-swift-\(UUID().uuidString)",
            isDirectory: true
        )

        do {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: false
            )
            defer {
                try? FileManager.default.removeItem(at: directory)
            }

            let expectedName = "visible-from-swift.txt"
            let file = directory.appendingPathComponent(expectedName)
            try Data().write(to: file)
            let nested = directory.appendingPathComponent("nested", isDirectory: true)
            try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: false)
            let nestedFile = nested.appendingPathComponent("child.txt")
            try Data().write(to: nestedFile)

            let client = Client()
            let session = try await client.connect(
                config: SessionConfig(
                    target: .local(serverExecutablePath: serverPath)
                )
            )
            require(
                session.target() == .local(serverExecutablePath: serverPath),
                "Session did not preserve its target"
            )
            let currentRoot = try await session.currentRoot()
            let expectedRoot = URL(
                fileURLWithPath: FileManager.default.currentDirectoryPath,
                isDirectory: true
            ).standardizedFileURL
            require(
                URL(fileURLWithPath: currentRoot, isDirectory: true).standardizedFileURL
                    == expectedRoot,
                "real server current root did not match its working directory"
            )

            let state = try await session.openState(path: directory.path)
            let independentState = try await session.openState(path: directory.path)
            let rootSnapshot = state.snapshot()
            let names = rootSnapshot.rows.map(\.entry.name)

            require(
                names.contains(expectedName),
                "real server listing did not return the test file"
            )
            let firstExpandedSnapshot = try await state.expand(path: nested.path)
            let firstNestedNames = firstExpandedSnapshot.rows
                .filter { $0.parentPath == nested.path }
                .map(\.entry.name)
            require(
                firstNestedNames == [nestedFile.lastPathComponent],
                "first shared-tree expansion returned unexpected contents"
            )

            require(
                independentState.snapshot() == rootSnapshot,
                "independent states on one session did not remain isolated"
            )

            let collapsedSnapshot = try state.collapse(path: nested.path)
            require(
                collapsedSnapshot.rows.allSatisfy { $0.parentPath != nested.path },
                "collapse did not discard nested rows"
            )

            let addedLater = nested.appendingPathComponent("added-later.txt")
            try Data().write(to: addedLater)
            let secondExpandedSnapshot = try await state.expand(path: nested.path)
            let secondNestedNames = Set(
                secondExpandedSnapshot.rows
                    .filter { $0.parentPath == nested.path }
                    .map(\.entry.name)
            )
            require(
                secondNestedNames == [nestedFile.lastPathComponent, addedLater.lastPathComponent],
                "second shared-tree expansion did not reload fresh contents"
            )
            require(
                state.snapshot() == secondExpandedSnapshot,
                "State.snapshot did not return the current browsing snapshot"
            )
            print("PASS session target and independent browsing states from Swift")
        } catch {
            fail("real server startup and listing failed: \(error)")
        }
    }

    private static func require(
        _ condition: @autoclosure () -> Bool,
        _ message: String
    ) {
        if !condition() {
            fail(message)
        }
    }

    private static func fail(_ message: String) -> Never {
        fputs("FAIL \(message)\n", stderr)
        exit(1)
    }
}
