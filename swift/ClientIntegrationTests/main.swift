import Darwin
import Foundation

@main
struct ClientIntegrationTests {
    static func main() async {
        testGeneratedValueTypes()
        await testAsyncTypedErrorRoundTrip()
        print("File Peeker client integration tests passed")
    }

    private static func testGeneratedValueTypes() {
        let config = ClientConfig(hostExecutablePath: "/tmp/file-peeker-host")
        require(
            config.hostExecutablePath == "/tmp/file-peeker-host",
            "ClientConfig did not preserve hostExecutablePath"
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
                config: ClientConfig(hostExecutablePath: "file-peeker-host")
            )
            fail("BrowserClient.start unexpectedly succeeded")
        } catch ClientError.NotImplemented(let operation) {
            require(
                operation == "BrowserClient.start",
                "received NotImplemented for the wrong operation"
            )
            print("PASS async typed error round trip")
        } catch {
            fail("received an unexpected error: \(error)")
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
