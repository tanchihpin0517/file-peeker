# Plan: Server Startup Routine for Local and Remote Browsing

## Objective

Implement one `Client.connect` lifecycle that can start a dedicated File
Peeker server in either of two locations:

- **Local:** spawn `file-peeker-server` on the client machine and connect directly
  to its private Unix domain socket.
- **Remote over SSH:** ask SSH to start `file-peeker-server` on the remote machine,
  forward a private local Unix socket to the server's private remote Unix socket,
  and use the same client/server protocol through that tunnel.

The UI-facing client API must select the target but must not expose process,
SSH, socket, or protocol details. Both modes must preserve the existing
one-client/one-server ownership model, the long-lived control connection, and one
short-lived connection per filesystem operation.

For remote targets, keep a private client installation helper that installs a
pinned `file-peeker-server` release from crates.io before startup. The helper is
compiled into the client for future lifecycle reuse but is currently invoked
only by internal Rust tests. It is not exported through Rust's public API or
UniFFI. Production installation must never use a Git repository or filesystem
path as the Cargo package source.

Local startup, directory listing, minimal local UIs, and drop-managed shutdown
are implemented. SSH startup and metadata remain planned work.

## Recommended design

Keep the protocol transport as a Unix domain stream in both modes. Remote mode
should not add a TCP listener to the server and should not introduce a second wire
protocol.

```text
Local

Session ── Unix socket ──> local file-peeker-server
      │
      └── owns local server child process and private socket directory

Remote over SSH

Session ── local Unix socket ──> SSH forward ──> remote Unix socket
      │                                                   │
      └── owns SSH child process                    remote file-peeker-server
```

This makes local and remote startup converge after launch: once the endpoint is
ready, control and operation connections use the same framing, handshake,
version checks, cancellation rules, and error mapping.

SSH is the recommended remote boundary because it already supplies server
authentication, user authentication, encryption, and remote process launch.
The server should continue to rely on the permissions of the account running it.
Restricting browsing to configured filesystem roots is a separate policy
feature and is not part of this startup change.

## Context

The repository is currently a compilable skeleton:

- `crates/file-peeker-client/src/lib.rs`
  - `SessionConfig` contains only `server_executable_path`.
  - `Client::connect` always returns `FilePeekerError::NotImplemented`.
  - `FilePeekerError` already has useful startup/lifecycle categories:
    `ServerStart`, `ServerExited`, `ConnectionClosed`, `Protocol`, and `Io`.
- `crates/file-peeker-server/src/main.rs`
  - `ServerConfig` contains one `socket_path`.
  - `parse_config` reads that path from the first positional argument.
  - `main`, `accept_control_connection`, and the operation handlers are
    placeholders.
- `crates/file-peeker-protocol/src/lib.rs`
  - Defines protocol version 1, control/operation roles, and the shared message
    types.
  - The protocol already permits multiple simultaneous connections to one
    dedicated server socket, which also works through an SSH Unix-socket forward.
- `docs/architecture.md`
  - Specifies a dedicated server per `Session`, a long-lived control
    connection, and one connection per operation.
  - Currently describes only local process ownership and explicitly defers
    remote support.
- `docs/protocol.md`
  - Specifies NDJSON over a private Unix domain socket.
  - Currently calls the protocol local-only and says remote use still needs a
    security design.
- `crates/file-peeker-tui/src/main.rs`
  - Does not call the client yet. It only reads an optional start path and
    prints the skeleton message.
- `swift/ClientIntegrationTests/main.swift`
  - Constructs the current `SessionConfig` and verifies the placeholder typed
    error crosses UniFFI.

The workspace uses Rust 2024 with Rust 1.88. `tokio` currently enables only
`macros` and `rt-multi-thread`; process management, Unix networking, buffered
I/O, timers, and synchronization will require the corresponding Tokio features
when implementation begins.

## Public configuration

Replace the single-path `SessionConfig` with a language-neutral target
configuration. Prefer a rich UniFFI enum so invalid local/remote field
combinations cannot be constructed.

Conceptually:

```rust
enum SessionTarget {
    Local {
        server_executable_path: String,
    },
    Ssh {
        destination: String,
        server_version: String,
        ssh_executable_path: String,
        ssh_arguments: Vec<String>,
    },
}

struct SessionConfig {
    target: SessionTarget,
    startup_timeout_ms: u64,
    shutdown_timeout_ms: u64,
}
```

Configuration rules:

- `destination` is an SSH destination such as `server.example.com` or
  `user@server.example.com`; reject an empty value or one beginning with `-`.
- `server_version` is an exact `MAJOR.MINOR.PATCH` package version, not a Cargo
  version requirement. Application defaults should normally select the server
  version shipped for the current client release.
- Use the fixed application-owned base `$HOME/.file-peeker/servers`; derive the
  executable as
  `<base>/<server_version>/bin/file-peeker-server`. A configurable arbitrary remote
  executable path is intentionally not part of the normal SSH target.
- Default `ssh_executable_path` to `ssh` in UI/application configuration rather
  than discovering it inside the protocol layer.
- Keep `ssh_arguments` as separate argv elements so callers can supply identity
  files, ports, jump servers, or other normal SSH options without building one
  shell command string.
- Do not accept a free-form remote command. Build the remote server command from
  the configured executable plus File Peeker-owned arguments.
- Add explicit, bounded startup and shutdown timeouts. Avoid hidden indefinite
  waits.
- Verify early that the chosen associated-value enum shape generates usable
  Swift through UniFFI 0.31.2. If that version cannot generate the desired
  shape, use separate `LocalServerConfig` and `SshServerConfig` records referenced
  by the enum rather than one record containing unrelated optional fields.

The TUI and SwiftUI should choose a target and pass it to the client. They
should not spawn the server or SSH themselves.

## Remote server installation

Installation and startup remain separate internal operations:

```text
Install:
client -> SSH -> cargo install from crates.io -> verify installed server

Startup:
client -> SSH -> invoke verified server -> forward socket -> handshake
```

Do not silently run `cargo install` during every `Client.connect`. The
private helper has no public or UI-facing entry point in this implementation.

### Production installation contract

The only supported production package source is crates.io. Install an exact
File Peeker version into an application-owned, versioned Cargo root:

```text
cargo install \
  --locked \
  --root "$HOME/.file-peeker/servers/0.1.0" \
  --version 0.1.0 \
  --bin file-peeker-server \
  file-peeker-server
```

Cargo installs the executable at:

```text
$HOME/.file-peeker/servers/0.1.0/bin/file-peeker-server
```

Use that exact path during remote startup. Do not depend on
`$HOME/.cargo/bin` being present in the non-interactive SSH `PATH`.

Installation rules:

- Pin the complete server version; do not use a caret, range, wildcard, Git
  revision, or "latest" selection.
- Use `--locked` so Cargo consumes the lockfile packaged with the binary crate.
- Install versions side by side. Do not overwrite one global server executable.
- Discover Cargo explicitly in the remote environment. Prefer an absolute path
  supplied by configuration or a small audited lookup such as
  `command -v cargo`; do not assume interactive shell initialization ran.
- Capture bounded stdout/stderr and impose a substantially longer installation
  timeout than the server startup timeout.
- After Cargo succeeds, invoke the installed binary's machine-readable version
  command and verify both the package version and supported protocol versions.
- Treat an already installed, successfully verified exact version as success
  without reinstalling it.
- Never add `--git` or `--path` as a production fallback. Failure to reach
  crates.io should remain an installation error.

The server CLI therefore needs a stable non-interactive command such as:

```text
file-peeker-server version --format json
```

Its output should contain at least:

```json
{
  "server_version": "0.1.0",
  "protocol_versions": [1]
}
```

Keep this bootstrap output on stdout and diagnostics on stderr.

### crates.io packaging requirements

Both `file-peeker-server` and its workspace dependency
`file-peeker-protocol` must be publishable crates:

- Add crates.io-required package metadata such as description, repository,
  readme, license, and appropriate include/exclude rules.
- Restrict publishing with `publish = ["crates-io"]`.
- Change the server dependency to contain both a registry version and a
  development path:

  ```toml
  file-peeker-protocol = {
      version = "=0.1.0",
      path = "../file-peeker-protocol"
  }
  ```

- Publish `file-peeker-protocol` first, wait until crates.io resolves it, and
  then publish `file-peeker-server`.
- Keep the server and protocol compatibility policy explicit. They may initially
  share a release version, but wire compatibility is ultimately determined by
  `PROTOCOL_VERSION`, not by assuming package versions are equal.
- Verify the packaged server, not only the workspace build, because Cargo removes
  path information and resolves the declared registry dependency when packaging
  and installing.

### Testing before crates.io publication

Use a simple package-install smoke test:

1. Run `cargo package` locally for `file-peeker-protocol` and
   `file-peeker-server`.
2. Transfer both `.crate` archives to a user-provided SSH target.
3. Unpack both archives in a temporary remote directory.
4. Run `cargo install --locked --path` against the unpacked server package, with
   a test-only `patch.crates-io.file-peeker-protocol.path` pointing to the
   unpacked protocol package.
5. Verify the executable and Cargo installation metadata.
6. Remove all temporary remote files.

This test proves that the packaged sources compile and install on a remote
machine. It deliberately does not simulate crates.io. Production installation
continues to use the pinned crates.io command described above.

## Shared startup state machine

`Client::connect` should use one bounded state machine for both targets:

```text
validate configuration
        │
        ▼
prepare private local endpoint and launch command
        │
        ▼
spawn owned child (server locally, SSH remotely)
        │
        ▼
retry connection while watching for child exit and startup timeout
        │
        ▼
open control connection and complete version/role handshake
        │
        ▼
return Session owning child, endpoint lease, and control connection
```

Recommended internal responsibilities:

- `prepare_launch(config)` validates the public configuration and returns a
  testable launch description: child executable/argv, the local socket path to
  connect to, temporary-directory ownership, and redacted diagnostics.
- `spawn_server(launch)` starts the child with stdin closed, stdout handled
  deliberately, and stderr captured into a diagnostic buffer.
- `connect_control(...)` retries only expected "not ready yet" connection
  failures. In parallel it watches child exit and the startup deadline.
- `handshake_control(...)` sends protocol version 1 with role `control` and
  requires `hello_ok` before startup succeeds.
- `RunningServer` owns the child process, the private local endpoint directory,
  stderr collection, and shutdown behavior.
- `Session` owns `RunningServer`, the control connection, and the shared
  closed/failed state used by later operations.

Do not introduce a public transport trait. Internally, either a small
`ServerLauncher` abstraction or an enum-backed launch description is acceptable,
but keep the common connection and handshake path concrete and shared. A pure
launch-description builder is useful for testing exact argv construction
without starting SSH.

## Local startup sequence

1. Validate that `server_executable_path` is non-empty.
2. Create a short, owner-only temporary directory and reserve
   `<directory>/server.sock`.
   - Account for the relatively small Unix socket path limit on macOS.
   - Prefer a deliberately short runtime base such as the system temporary
     directory rather than nesting under a long application path.
   - The socket file must not already exist.
3. Spawn the local server using explicit arguments, for example:

   ```text
   file-peeker-server serve --socket /tmp/fp-<random>/server.sock
   ```

4. Close or null the child's stdin. Reserve stdout for a future machine-readable
   bootstrap protocol, or null it for this implementation. Capture stderr with
   a strict size bound so an unhealthy child cannot consume unlimited memory.
5. Attempt the control connection until one of these terminal outcomes:
   - connect succeeds and the control handshake succeeds;
   - the child exits;
   - the startup timeout expires;
   - a non-retryable socket or protocol error occurs.
6. On failure, terminate/reap the child, remove the socket directory, and return
   a typed error containing the executable, exit status when known, and bounded
   stderr context.
7. On success, retain the child and temporary-directory lease inside
   `Session`.

The client should not treat appearance of the socket file alone as readiness.
The successful control handshake is the readiness signal.

## Remote SSH startup sequence

1. Validate the SSH executable, destination, exact server version, derived remote
   server executable, and SSH argument list.
2. Create a short, owner-only **local** temporary directory and reserve a local
   forwarded socket path such as `/tmp/fp-<random>/server.sock`.
3. Generate a separate unpredictable **remote** runtime directory and socket
   path, also short enough for Unix socket limits, such as
   `/tmp/fp-<random>/server.sock`.
4. Build an SSH argv vector equivalent to:

   ```text
   ssh \
     <configured ssh arguments> \
     -o ExitOnForwardFailure=yes \
     -o StreamLocalBindUnlink=yes \
     -L <local-socket>:<remote-socket> \
     <destination> \
     <escaped remote server command>
   ```

   The remote server command should be equivalent to:

   ```text
   file-peeker-server serve \
     --socket <remote-socket> \
     --create-private-parent \
     --remove-private-parent-on-exit
   ```

5. Construct the remote command with one audited POSIX-shell argument-escaping
   helper. OpenSSH normally passes the remote command through the user's remote
   shell even when the local process was given separate argv elements. Never
   interpolate unescaped destination, executable, or socket strings.
6. Keep the SSH process in the foreground as the owned child. It represents
   both the remote server lifetime and the forwarding tunnel.
7. Connect the File Peeker control connection to the **local forwarded socket**.
   Reuse the same retry, child-exit, timeout, and protocol-handshake logic as
   local startup.
8. On failure, terminate/reap SSH and remove the local endpoint. The remote server
   must remove its socket/runtime directory during normal exit and should also
   clean stale socket state when it receives termination caused by SSH loss.
9. On success, retain the SSH child and local temporary-directory lease inside
   `Session`. Every later operation opens another connection to the same
   local forwarded socket; SSH forwards each connection to the one remote server.

Do not expose the remote socket over TCP. Do not disable SSH server-key checking.
Interactive SSH password/passphrase prompts are not a reliable GUI startup
mechanism; initial implementation should expect keychain/agent/non-interactive
authentication and surface SSH's complete stderr when authentication fails.

## Server command-line and endpoint ownership

Update `crates/file-peeker-server/src/main.rs` so the server has an explicit,
testable command-line contract rather than one unnamed positional path.

Required behavior:

- Use Clap for command parsing, generated help, standard invalid-input
  diagnostics, and the top-level package version flag.
- `serve --socket <path>` listens at a supplied Unix socket.
- `version --format json` reports machine-readable server and protocol
  versions; `--version` reports the package version for humans.
- Local mode expects the parent directory to exist and be private.
- `--create-private-parent` atomically creates the socket's parent with
  owner-only permissions and fails if it already exists. This is intended for
  the generated remote runtime directory.
- `--remove-private-parent-on-exit` removes the socket and the parent directory
  only when the server created/owns that directory.
- Reject an existing socket path instead of silently unlinking an arbitrary
  filesystem object.
- Validate that the resulting socket path is absolute and within platform
  length limits.
- Bind the listener before accepting connections.
- Accept exactly one control connection before operation connections, as
  already specified by `docs/protocol.md`.
- On control connection loss, close active operation connections, remove owned
  endpoint state, and exit.
- Handle normal termination signals so SSH shutdown does not leave the remote
  runtime directory behind.

Keep filesystem browsing permissions equal to the account running the server.
Endpoint creation secures client/server transport ownership; it is not a browsing
root restriction.

## Error and cleanup contract

Map startup failures consistently:

- Invalid target, executable path, destination, timeout, or socket path:
  `FilePeekerError::ServerStart`.
- Failure to spawn the local server or SSH:
  `FilePeekerError::ServerStart`.
- Child exits before or during the initial handshake:
  `FilePeekerError::ServerExited`.
- Startup deadline expires:
  add a distinct timeout variant or return `ServerStart` with an explicit timeout
  message; a distinct variant is preferable if UIs need tailored messaging.
- Unsupported protocol version or invalid handshake:
  `FilePeekerError::Protocol`.
- Control connection disappears after successful startup:
  `FilePeekerError::ConnectionClosed` and invalidate all operations.

Diagnostics must:

- distinguish local server exit from SSH exit;
- include exit status and complete stderr;
- avoid logging secrets or an entire user SSH configuration;
- display argv in a redacted/debug-safe form;
- preserve the primary startup error if cleanup also fails.

Shutdown must be idempotent:

1. Mark the client closed so no new operations start.
2. Close the control connection.
3. Allow a short graceful-exit window.
4. If the owned child is still alive, terminate it.
5. Wait/reap it so no zombie remains.
6. Remove the local socket and temporary directory.

Because Rust `Drop` cannot await, put asynchronous shutdown in an internal task
or runtime-owned lifecycle worker. `Drop` should signal that worker and provide
a non-blocking last-resort child kill path. An explicit public `close()` may be
added if deterministic error-reporting is required, while `Drop` remains the
safety net.

## Scope

### In scope

- Public client configuration for local and SSH targets.
- One shared startup state machine.
- Local server process launch.
- Remote server launch through SSH Unix-socket forwarding.
- Private remote server installation from crates.io, exercised only by internal
  Rust tests.
- Remote package-install smoke test for unpublished server releases.
- Private local and remote endpoint creation/cleanup.
- Initial control connection and version handshake.
- Startup timeout, early-exit detection, diagnostics, and owned-child cleanup.
- Unit and integration tests for both launch modes.
- Updating architecture, protocol, README, Rust, and Swift integration docs/tests
  when implementation occurs.

### Out of scope

- Raw TCP server listeners.
- A custom TLS/authentication protocol.
- Password-entry or SSH credential-management UI.
- Git-, path-, archive-, or ad hoc binary-based production installation.
- Automatically installing the server as a side effect of every startup.
- SSH connection pooling across multiple `Session` instances.
- Automatic reconnection after SSH or server failure.
- Sharing one server among multiple clients.
- Filesystem allowlists/chroot/sandbox policy.
- Directory listing and metadata implementation except for enough protocol
  behavior to prove startup and lifecycle.
- Linux and Windows support.

## Implementation steps

1. **Lock down the startup contract in documentation.**
   - Update `docs/architecture.md` to describe `SessionTarget`, the shared startup
     state machine, local ownership, SSH forwarding, and shutdown behavior.
   - Update `docs/protocol.md` to clarify that NDJSON remains on Unix streams;
     remote use is the same private protocol carried through authenticated SSH,
     not a network-exposed server service.
   - Update `README.md` so local and remote capability claims match the actual
     implementation stage.

2. **Prepare the server and protocol crates for crates.io packaging.**
   - Add complete package metadata and restrict publishing to crates.io.
   - Give `file-peeker-protocol` a registry version alongside its workspace
     path in the server manifest.
   - Add package-content checks and build/test the generated `.crate` archives.
   - Document and automate the protocol-first, server-second release order.

3. **Add the private remote installation helper.**
   - Build the pinned `cargo install` argv without a shell command string.
   - Install into an application-owned versioned root.
   - Compile it normally for future reuse, but call it only from internal Rust
     tests and export no Rust, UniFFI, Swift, or UI surface.
   - Add bounded diagnostic capture and an installation timeout.
   - Add `file-peeker-server version --format json` and verify its result after
     installation.
   - Keep the production install command hard-coded to crates.io semantics.

4. **Expand public configuration and generated-binding tests.**
   - In `crates/file-peeker-client/src/lib.rs`, add the local/SSH target types,
     startup/shutdown timeouts, and any new typed errors.
   - Update the Rust tests around `Client::connect`.
   - Update `swift/ClientIntegrationTests/main.swift` to construct both target
     variants and verify their values/errors survive UniFFI.
   - Run binding generation immediately to catch unsupported UniFFI enum shapes
     before building internal startup code.

5. **Add protocol framing and connection handshake primitives.**
   - Implement NDJSON send/receive around the types in
     `crates/file-peeker-protocol/src/lib.rs`, either in that crate or in small
     server/client transport modules with shared framing tests.
   - Enforce required hello ordering, version 1, and connection roles.
   - Keep the framing independent of how the Unix stream became reachable.

6. **Implement the server CLI and private endpoint lifecycle.**
   - Replace `parse_config` in `crates/file-peeker-server/src/main.rs` with the
     explicit `serve --socket ...` contract.
   - Add safe private-parent creation for remote launch and ownership-aware
     cleanup.
   - Bind the Unix listener and implement enough control lifecycle to accept
     `hello`, return `hello_ok`, remain alive, and exit when control closes.
   - Keep operation handlers stubbed behind explicit errors if listing and
     metadata are still scheduled separately.

7. **Implement the client's launch preparation layer.**
   - Move startup internals out of the exported API block into focused modules,
     for example `client/startup.rs`, `client/endpoint.rs`, and
     `client/connection.rs`.
   - Build and unit-test local and SSH launch descriptions.
   - Add the audited POSIX-shell escaping helper and table-driven edge-case
     tests.
   - Add short socket-path generation and owner-only temporary-directory
     handling.

8. **Implement child supervision and server diagnostics.**
   - Spawn the selected child with controlled stdio.
   - Capture complete stderr without blocking the child.
   - Race connection retries against child exit and startup timeout.
   - On every failure path, terminate/reap the child and release endpoint state.

9. **Complete `Client::connect`.**
   - Use the shared state machine for both target variants.
   - Connect to the prepared local endpoint, perform the control handshake, and
     return a thread-safe client owning `RunningServer`.
   - Record a single terminal client failure when the child or control
     connection later dies so all operations observe consistent invalidation.

10. **Implement graceful and forced shutdown.**
   - Closing/dropping `Session` closes control first.
   - Wait only up to the configured shutdown timeout.
   - Kill and reap a server/SSH process that does not exit.
   - Verify local and remote endpoint cleanup, including cancellation during
     startup.

11. **Build the remote package-install smoke test.**
   - Package the protocol and server crates as release artifacts.
   - Transfer and unpack both packages on a configured SSH target.
   - Install the server from its packaged path with a test-only path patch for
     the unpublished protocol package.
   - Verify Cargo installation metadata and the installed executable.
   - Clean the remote fixture after success or failure.

12. **Wire the application shells only after the client contract is stable.**
   - Update `crates/file-peeker-tui/src/main.rs` to construct a local or SSH
     target from its eventual CLI options and call `Client::connect`.
   - Update the Swift shell/model when it gains functional startup UI.
   - Keep all process and SSH logic inside `file-peeker-client`.

## Verification

### Unit tests

- Local launch argv uses the configured server path and one absolute short socket
  path.
- SSH launch argv includes `ExitOnForwardFailure`, Unix-socket forwarding, the
  destination, and a correctly escaped remote command.
- Shell escaping covers empty strings, spaces, quotes, Unicode, and leading
  hyphens.
- Invalid destinations and empty executable paths fail before spawning.
- Endpoint paths obey the target platform's Unix socket path limit.
- Existing files/sockets are never removed unless owned by the current startup.
- Framing rejects malformed, out-of-order, and wrong-version messages.
- Complete stderr capture retains all server diagnostics.
- Production installer argv contains no `--git`, `--path`, `--index`, or
  alternate `--registry` option.
- The unpublished smoke test uses `--path` only after transferring and unpacking
  the packaged sources.
- The server and protocol `.crate` archives contain all required source,
  metadata, lockfile, and runtime assets.

### Local integration tests

- Build `file-peeker-server`, start `Session` locally, complete the control
  handshake, and confirm the server remains alive.
- Drop/close the client and confirm the server exits and the temporary directory
  disappears.
- Use a fake server that exits immediately and assert `ServerExited` includes its
  status/stderr.
- Use a fake server that never binds and assert startup times out and the child is
  reaped.
- Use a wrong-version fake server and assert a protocol error plus cleanup.

### Remote integration tests

Use an opt-in test target such as an SSH server in a container or a dedicated
loopback SSH fixture; do not make ordinary unit tests depend on a developer's
personal SSH configuration.

- Transfer and unpack the unpublished packaged protocol and server.
- Install the server from the packaged path with the protocol path patch.
- Verify Cargo reports the exact installed version and executable.
- Start a remote server through SSH and complete the same control handshake.
- Open multiple simultaneous operation connections through the one forwarded
  socket.
- Close control and confirm the remote server and SSH process exit.
- Test authentication failure, missing remote executable, forward setup
  failure, remote early exit, and startup timeout.
- Confirm no TCP listener is opened and remote socket/runtime state is removed.

### Repository checks

Run:

```text
make check
make bindings
make client-integration-test
make runnable-test
```

Run the opt-in SSH integration suite separately with an explicitly configured
fixture. Once the SwiftUI shell consumes the new configuration, also run:

```text
make xcode-build
```

### Acceptance criteria

- One `Client.connect` API starts either a local server or an SSH-based
  remote server based only on `SessionConfig`.
- Production installation obtains an exact pinned server version only from
  crates.io.
- An unpublished packaged release can be transferred and installed over SSH
  without using Git or simulating crates.io.
- Both modes complete the same control handshake and use the same operation
  connection code after startup.
- The remote server is reachable only through SSH Unix-socket forwarding.
- Startup never waits forever and reports early child exit with useful bounded
  diagnostics.
- Failed startup and normal shutdown leave no owned child process or local
  endpoint behind.
- Normal remote shutdown removes the remote endpoint directory.
- Rust and generated Swift can construct both target configurations.
- UIs contain no direct process, SSH, socket, or protocol management.

## Open questions and assumptions

1. **Remote transport assumption:** this plan defines "remote" as SSH. If remote
   must support a daemon reached directly over a network, the authentication,
   encryption, discovery, and authorization design must be decided before
   implementation; that is a materially different architecture.
2. **Remote Cargo prerequisite:** installation assumes a compatible Rust/Cargo
   toolchain exists on the remote machine and can reach crates.io in production.
   Bootstrapping Rust itself is outside this plan.
3. **SSH interaction:** this plan assumes authentication can complete without a
   terminal prompt. A future UI may integrate an SSH askpass flow, but startup
   must not hang waiting on an invisible prompt.
4. **Platform support:** Unix-socket forwarding requires an OpenSSH version with
   stream-local forwarding support on both ends. Initial support remains
   macOS client plus a Unix-like remote server.
5. **Allowed roots:** the remote server can read everything permitted to the SSH
   account. If File Peeker needs a narrower security boundary, add an explicit
   server-side allowed-root configuration as a separate, reviewed feature.
6. **Public close method:** decide whether UI callers need an awaitable
   `Session.close()` that can report shutdown errors, or whether
   best-effort drop semantics are sufficient for the first functional version.
