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

UI -> Session -> local Unix socket -> SSH master -> remote Unix socket
                         |                 |              -> remote server
                         |                 |              -> remote filesystem
                         |                 |
                         +-> control socket +-> one authenticated SSH transport
```

The important design choice is that remote browsing does not introduce a TCP
server or a second protocol. SSH supplies authentication, encryption, remote
process launch, and Unix-socket forwarding. After the local server or remote
forwarding is ready, both targets converge on the same connection and protocol
behavior.

Each `Session` owns exactly one dedicated server lifecycle:

- one long-lived control connection defines the shared lifetime;
- each filesystem operation uses its own short-lived connection;
- closing the control connection asks the dedicated server to exit;
- dropping the last session/listing reference or explicitly closing the session
  cleans up its owned process and private endpoints.

Local and SSH startup share endpoint, handshake, diagnostic, and lifecycle
primitives, but each target owns its process-specific orchestration. Their
common endpoint sequence is:

```text
select and validate target
        |
        v
prepare a private local endpoint
        |
        v
launch the local server or prepare the SSH transport
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

For SSH targets, the client creates one ControlMaster before checking or
installing the server. Compatibility checks, installation, server launch, and
StreamLocal forwarding all reuse that authenticated SSH transport. The client
selects the matching server version, keeps it in an application-owned versioned
directory, and hides installation details from the UI. Development and release
builds differ only in package source: transferred workspace packages for
development and crates.io for release.

## Internal module layout

Server ownership and launch code lives under
`crates/file-peeker-client/src/server/`:

- `mod.rs` is the server-ownership facade. It dispatches `SessionTarget`, owns
  `ServerHandle`, and marks the server closed after its target-specific
  supervisor finishes.
- `local.rs` owns the local child process, startup rollback, and supervision.
- `remote.rs` orchestrates the SSH master, installation, server launcher,
  forwarding, rollback, and remote supervision.
- `ssh.rs` owns OpenSSH command construction and control operations.
- `runtime.rs` owns session directories, socket paths, permissions, UUIDs, and
  log retention.
- `protocol.rs` owns socket connection retries and the control handshake.
- `diagnostics.rs` owns bounded stderr capture and session-log writing.

Local and remote process structs remain private to their modules. Each target
passes its supervisor future to `ServerHandle`; there is no shared process enum
and no process-specific branching in the server-ownership facade.

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
2. Creates an owner-only session directory under
   `~/.file-peeker/run/<session-id>/` and reserves `server.sock` inside it.
3. Starts the server with:

   ```text
   file-peeker-server serve --socket <private-socket>
   ```

4. Drains server stderr, retains bounded diagnostics in memory, and writes the
   full stream to `~/.file-peeker/logs/<session-id>.log`.
5. Retries the socket connection while also watching for process exit and the
   startup deadline.
6. Sends the protocol-version and control-role handshake.
7. Returns a `Session` that owns the server process, control connection,
   and session endpoint.

The Ratatui application and SwiftUI application currently construct local
targets. The Swift application bundles the server executable in its application
resources.

### Local server startup timeline

```text
Client                   Local server
  |                            |
  |-- create session dir ----->|  ~/.file-peeker/run/<session-id>/
  |-- spawn ------------------>|  serve --socket server.sock
  |                            |-- bind server.sock
  |-- connect with retry ----->|
  |-- hello(control, version)->|
  |<--------- hello_ok --------|
  |                            |
  +-- return ready Session     +-- accept operation connections
```

The server is considered ready only after it has bound `server.sock`, accepted
the dedicated control connection, and returned `hello_ok` for the expected
protocol version. Merely spawning the process or observing the socket file is
not sufficient.

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

1. Creates `~/.file-peeker/run/<session-id>/` locally with `server.sock` and
   `cm.sock` paths, rejecting unsafe ownership, symlinks, or paths that cannot
   be encoded for Unix sockets and StreamLocal forwarding.
2. Starts a foreground OpenSSH ControlMaster and waits up to 300 seconds for
   authentication and `ssh -O check` readiness.
3. Checks the matching remote server installation through the master and
   installs only when required. Every install command uses the same
   `ControlPath` with non-interactive multiplex clients.
4. Queries the absolute remote home path, then reserves
   `~/.file-peeker/run/<session-id>/server.sock` on the remote host.
5. Starts a long-lived SSH launcher channel through the master. The remote
   wrapper creates the owner-only runtime directory, starts the installed
   server, monitors launcher stdin, and removes its directory on exit.
6. Adds forwarding separately with `ssh -O forward`, a StreamLocal mapping from
   the local socket to the remote socket, and `ExitOnForwardFailure=yes`.
7. Connects to the local forwarded socket within the five-second startup
   deadline and performs the same control handshake used by local startup.
8. Returns the same `Session` type used for a local target.

The server is not exposed on a TCP port. Authentication and encryption are
provided by SSH, and normal SSH configuration continues to control identities,
ports, jump hosts, and host verification. The master is dedicated to one
`Session`; it does not reconnect or persist after that session closes.

### Remote server startup timeline

```text
Client             SSH master          Remote launcher       Remote server
  |                     |                       |                    |
  |-- start/auth ------>|                       |                    |
  |-- -O check -------->|                       |                    |
  |-- install/check --->|-- exec channels ---->|                    |
  |-- query HOME ------>|-- exec channel ----->|                    |
  |-- launch ---------->|-- session channel -->|                    |
  |                     |                       |-- create runtime ->|
  |                     |                       |-- spawn ---------->|
  |                     |                       |                    |-- bind server.sock
  |-- -O forward ------>|                       |                    |
  |-- connect local --->|== StreamLocal forwarding ================>|
  |-- hello(control) -->|===========================================>|
  |<------------------------------- hello_ok -----------------------|
  |                     |                       |                    |
  +-- return ready Session                     |                    +-- accept operations
```

The master must be ready before installation begins. The launcher starts the
remote server without daemonizing it and keeps stdin open as its lifetime
signal. StreamLocal forwarding is added only through the existing master. As
with local startup, the remote session becomes ready only after the forwarded
control handshake succeeds.

## Operations after startup

The long-lived control connection defines the lifetime shared by the client and
its dedicated server. Filesystem operations open separate connections through
the same local or forwarded socket and perform an operation-role handshake.

The implemented operations are:

- stream directory-entry batches through pull-based `Listing` objects;
- return the server's current working directory for the diagnostic CLI.

The metadata client method is still a typed `NotImplemented` result.

## Shutdown and cleanup

`Session::close` and `Session` drop both signal the lifecycle
supervisor. Shutdown:

1. Closes the control connection.
2. Lets the local server exit within a bounded grace period, or closes the
   remote launcher stdin and lets its cleanup trap stop the remote server and
   remove the remote runtime directory.
3. Kills and reaps an owned server or launcher that does not exit in time.
4. For SSH startup, cancels the StreamLocal forward with `ssh -O cancel`, asks
   the master to exit with `ssh -O exit`, and kills it if bounded shutdown does
   not complete.
5. Drops the owned local session directory, removing its sockets.

Explicit close is idempotent. New operations are rejected after the lifecycle
has closed. `Session::close().await` waits for the supervisor; `Drop` sends the
same shutdown signal but cannot wait synchronously.

## Runtime files and diagnostics

Runtime and log roots are maintained separately:

```text
~/.file-peeker/
├── run/<session-id>/
│   ├── server.sock
│   └── cm.sock        # SSH sessions only
└── logs/<session-id>.log
```

The `run` root and session directories use mode `0700`; log files use `0600`.
The client retains the ten newest session logs. Normal shutdown removes the
session directory, while logs remain available for diagnosis. Automatic
cleanup after process crashes or machine restarts is intentionally deferred.

## Diagnostic CLI

The client crate provides Clap commands for remote connection and installation,
local or remote directory listing, and local file opening. They emit verbose
progress to stderr.

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

`list` prints the absolute paths of a directory's direct children, one per
stdout line. It lists locally by default; pass `--remote` to use an SSH target:

```text
cargo run -p file-peeker-client -- list PATH
cargo run -p file-peeker-client -- list --remote SSH_DESTINATION PATH
```

Its final stderr diagnostic reports entry and batch counts, elapsed listing
time, and entries processed per second; stdout remains paths only.

## Verification completed

The implemented routine is covered by:

- Rust formatting, compilation, Clippy, unit tests, and workspace tests;
- a real local server startup, listing, shutdown, and cleanup test;
- a non-interactive local Ratatui smoke test;
- UniFFI generation and Swift tests for both `SessionTarget` variants;
- a Swift client test that starts the bundled server and lists a directory;
- an Xcode build of the SwiftUI application;
- a remote package installation script for unpublished crates;
- unit coverage for runtime-path security, socket limits, SSH multiplex
  arguments, protocol framing, and diagnostics.

Run the complete local verification sequence with:

```text
make verify
```

Run the unpublished remote installation test with:

```text
scripts/test-remote-server-install.sh SSH_DESTINATION
```

When an SSH destination is available, run the complete remote lifecycle
manually with:

```text
cargo run -p file-peeker-client -- connect SSH_DESTINATION
```

## Remaining startup work

The main remaining work is hardening rather than the happy-path startup flow:

- automated remote lifecycle and failure-path integration tests;
- crash recovery and stale runtime-file cleanup;
- local timeout, wrong-version, and diagnostic-cleanup integration fixtures;
- preservation of one terminal lifecycle error for consistent later failures;
- crates.io publishing metadata and validation of the release installation
  path after publication;
- optional remote-target selection in the Ratatui and SwiftUI applications;
- reconciliation of older planning and architecture text with the implemented
  behavior.

The original design and its broader test matrix remain in
[`startup-routine-plan.md`](startup-routine-plan.md).
