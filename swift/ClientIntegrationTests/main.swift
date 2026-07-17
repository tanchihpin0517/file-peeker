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
        let localConfig = ClientConfig(
            target: .local(serverExecutablePath: "/tmp/file-peeker-server")
        )
        require(
            localConfig.target
                == .local(serverExecutablePath: "/tmp/file-peeker-server"),
            "ClientConfig did not preserve its local target"
        )

        let sshConfig = ClientConfig(target: .ssh(destination: "example-host"))
        require(
            sshConfig.target == .ssh(destination: "example-host"),
            "ClientConfig did not preserve its SSH target"
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
            _ = try await BrowserClient.start(
                config: ClientConfig(
                    target: .local(
                        serverExecutablePath: "/definitely/missing/file-peeker-server"
                    )
                )
            )
            fail("BrowserClient.start unexpectedly succeeded")
        } catch ClientError.ServerStart(let message) {
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

            let client = try await BrowserClient.start(
                config: ClientConfig(
                    target: .local(serverExecutablePath: serverPath)
                )
            )
            let listing = try await client.startListing(path: directory.path)
            var names: [String] = []
            while let entry = try await listing.nextEntry() {
                names.append(entry.name)
            }

            require(
                names.contains(expectedName),
                "real server listing did not return the test file"
            )
            print("PASS real server startup and listing from Swift")
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
