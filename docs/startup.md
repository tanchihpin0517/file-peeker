# Startup routine

Status: local and SSH startup are implemented for the shared client. Both use
one public startup API and the same private Unix-socket protocol after the
server is running.

## Overall idea

The startup routine lets a UI browse files through one shared client regardless
of whether the filesystem is on the current machine or an SSH destination. The
UI chooses only a target. Process management, installation, SSH, sockets,
protocol negotiation, and cleanup remain private client responsibilities.

```text
Local

UI -> Session -> private Unix socket -> local server -> local filesystem
                         client owns process and endpoint

Remote

UI -> Session -> local Unix socket -> SSH forward -> remote Unix socket
                         client owns SSH              -> remote server
                                                      -> remote filesystem
```

The important design choice is that remote browsing does not introduce a TCP
server or a second protocol. SSH supplies authentication, encryption, remote
process launch, and Unix-socket forwarding. After the local server process or
remote SSH process has been launched, both targets converge on the same
connection and protocol behavior.

Each `Session` owns exactly one dedicated server lifecycle:

- one long-lived control connection defines the shared lifetime;
- each filesystem operation uses its own short-lived connection;
- closing the control connection asks the dedicated server to exit;
- dropping the last session/state reference or explicitly closing the session cleans up its owned process and
  private endpoints.

Startup follows the same bounded sequence for either target:

```text
select and validate target
        |
        v
prepare a private local endpoint
        |
        v
launch the local server or SSH process
        |
        v
retry connection while watching timeout and child exit
        |
        v
complete protocol-version and control-role handshake
        |
        v
return Session owning the complete lifecycle
```

For SSH targets, compatible server installation is a preparation step before
launch. The client selects the matching server version, keeps it in an
application-owned versioned directory, and hides installation details from the
UI. Development and release builds differ only in package source: transferred
workspace packages for development and crates.io for release.

## Public client API

The shared API starts either target through `Client::connect`:

```rust
Client::new().connect(SessionConfig {
    target: SessionTarget::Local {
        server_executable_path,
    },
})
```

```rust
Client::new().connect(SessionConfig {
    target: SessionTarget::Ssh {
        destination,
    },
})
```

There is no separate public remote-connect function. `SessionTarget` is exported
through UniFFI, so Rust and Swift callers use the same local/SSH configuration.

The SSH destination is required and has no default. It may be a host alias such
as `ntu` from the user's SSH configuration. Empty destinations and values that
begin with `-` are rejected.

## Local startup

For a local target, the client:

1. Validates that the server executable path is present.
2. Creates an owner-only temporary directory under `/tmp` and reserves a short
   Unix socket path inside it.
3. Starts the server with:

   ```text
   file-peeker-server serve --socket <private-socket>
   ```

4. Captures bounded server diagnostics from stderr.
5. Retries the socket connection while also watching for process exit and the
   startup deadline.
6. Sends the protocol-version and control-role handshake.
7. Returns a `Session` that owns the server process, control connection,
   and temporary endpoint.

The Ratatui application and SwiftUI application currently construct local
targets. The Swift application bundles the server executable in its application
resources.

## Remote installation

Remote servers are installed below:

```text
~/.file-peeker/servers/<version>/bin/file-peeker-server
```

Installation has two sources but one implementation:

- Debug/development builds package the workspace protocol and server crates,
  transfer the `.crate` archives over SSH, and install the packaged server with
  a test-only path patch for the unpublished protocol crate.
- Release builds install the exact matching `file-peeker-server` version from
  crates.io using `cargo install --locked`.

The installer verifies that the executable exists and that
`version --format json` reports the expected server and protocol versions. The
installation helper remains private to the client crate and is not exported as
a Rust library or UniFFI function.

Cleanup of temporary development packages is attempted after both successful
and failed installation attempts. A cleanup failure makes an otherwise
successful installation fail.

## Remote startup

For an SSH target, `Client::connect`:

1. Checks whether the matching remote server is already installed and
   compatible.
2. Installs it only when installation is required; a compatible installation is
   not overwritten.
3. Creates a private local Unix socket endpoint.
4. Starts SSH with Unix-socket forwarding and
   `ExitOnForwardFailure=yes`.
5. Creates a private remote runtime directory and starts the installed server
   on a Unix socket inside it.
6. Connects to the local end of the forwarded socket and performs the same
   control handshake used by local startup.
7. Returns the same `Session` type used for a local target.

The server is not exposed on a TCP port. Authentication and encryption are
provided by SSH, and normal SSH configuration continues to control identities,
ports, jump hosts, and host verification.

## Operations after startup

The long-lived control connection defines the lifetime shared by the client and
its dedicated server. Filesystem operations open separate connections through
the same local or forwarded socket and perform an operation-role handshake.

The implemented operations are:

- collect directory entries while loading or expanding a browsing state;
- return the server's current working directory for the diagnostic CLI.

The metadata client method is still a typed `NotImplemented` result.

## Shutdown and cleanup

`Session::close` and `Session` drop both signal the lifecycle
supervisor. Shutdown:

1. Closes the control connection.
2. Lets the server or SSH process exit within a bounded grace period.
3. Kills and reaps the owned process if it does not exit in time.
4. Removes the owned local temporary socket directory.
5. For SSH startup, closes SSH stdin and removes the private remote runtime
   directory through the remote cleanup trap.

Explicit close is idempotent. New operations are rejected after the lifecycle
has closed.

## Diagnostic CLI

The client crate provides two Clap commands. Both require an explicit SSH
destination and emit verbose progress to stderr.

Normal startup checks and reuses a compatible installation:

```text
cargo run -p file-peeker-client -- connect SSH_DESTINATION
```

`connect` starts the remote server, prints its current root to stdout, and
closes the connection.

Forced installation always overwrites the versioned installation:

```text
cargo run -p file-peeker-client -- install SSH_DESTINATION
```

`install` prints the installed executable path to stdout after verification.

## Verification completed

The implemented routine is covered by:

- Rust formatting, compilation, Clippy, unit tests, and workspace tests;
- a real local server startup, listing, shutdown, and cleanup test;
- a non-interactive local Ratatui smoke test;
- UniFFI generation and Swift tests for both `SessionTarget` variants;
- a Swift client test that starts the bundled server and lists a directory;
- an Xcode build of the SwiftUI application;
- a remote package installation script for unpublished crates;
- an end-to-end SSH diagnostic run against the configured `ntu` destination,
  including installation compatibility checking, control handshake, current
  root retrieval, and clean shutdown.

Run the complete local verification sequence with:

```text
make verify
```

Run the unpublished remote installation test with:

```text
scripts/test-remote-server-install.sh SSH_DESTINATION
```

## Remaining startup work

The main remaining work is hardening rather than the happy-path startup flow:

- automated remote lifecycle and failure-path integration tests;
- local timeout, wrong-version, and diagnostic-cleanup integration fixtures;
- preservation of one terminal lifecycle error for consistent later failures;
- crates.io publishing metadata and validation of the release installation
  path after publication;
- optional remote-target selection in the Ratatui and SwiftUI applications;
- reconciliation of older planning and architecture text with the implemented
  behavior.

The original design and its broader test matrix remain in
[`startup-routine-plan.md`](startup-routine-plan.md).
