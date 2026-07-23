# Client CLI

`file-peeker-client` is a test-only command-line tool for exercising the native
local backend and remote server provisioning, startup, and gRPC connections.

## `test` subcommands

All test-only operations are nested under the top-level `test` command.

### `test connect [--force] SERVER`

Ensures the server is installed from Git on `SERVER`, starts it through SSH,
authenticates a persistent gRPC channel, checks the standard Health service,
and prints the port and token as one line of JSON. `--force` reinstalls the
remote server first. Exiting closes the server's stdin lifetime lease.

```text
cargo run -p file-peeker-client -- test connect example.test
cargo run -p file-peeker-client -- test connect --force example.test
```

### `test install [--force] [--from-source PATH] SERVER`

Checks for the matching server executable on `SERVER` and attempts to install
it from Git with Cargo when it is missing. The resolved remote executable path
is written to local standard output. Use `--force` to reinstall it.

`--from-source PATH` selects an independent source-install workflow. The
command uploads the Git-aware contents of local `PATH` to
`.file-peeker/debug/repo` and installs that uploaded package over SSH. This
option does not change normal connection provisioning, which remains Git-only.

```text
cargo run -p file-peeker-client -- test install example.test
cargo run -p file-peeker-client -- test install --force example.test
cargo run -p file-peeker-client -- test install --force --from-source /path/to/file-peeker example.test
```

### `test ssh-connection SERVER`

Connects to `SERVER`, sends `echo 'connect to SERVER'` through SSH standard
input, and writes only the last line of remote output to local standard output.

```text
cargo run -p file-peeker-client -- test ssh-connection example.test
```

### `test start-server SERVER`

Force-installs the matching server executable from Git on `SERVER`, starts it,
and prints its port and token as one line of JSON. The test closes the SSH input
after startup, which stops the remote server before the command exits.

```text
cargo run -p file-peeker-client -- test start-server example.test
```

### `test list PATH [--remote SERVER]`

Uses the in-process filesystem core to list the direct children of `PATH`.
Relative paths are resolved from the client's working directory. Each
child's path is written to standard output on its own line; output order follows
the selected host's native filesystem iteration order. Shell expressions are
preserved in the output while `~` and `$VARIABLES` are expanded in the client
environment for local listing. After the listing completes, a debug line on
standard error reports the entry count, elapsed milliseconds, and entries per
second.

With `--remote SERVER`, the matching server is started through SSH and the same
listing operation runs on that host. Relative paths are resolved from the
remote server's working directory, and shell expressions are expanded in the
remote server's environment.

```text
cargo run -p file-peeker-client -- test list .
cargo run -p file-peeker-client -- test list /tmp/report-drafts
cargo run -p file-peeker-client -- test list . --remote example.test
```

### `test open PATH [--remote SERVER]`

Resolves `PATH` on the selected host and opens the regular file with the
client operating system's default application. Local files are opened at their
resolved path. With `--remote SERVER`, the file is completely and atomically
downloaded into the client cache before it is opened. The system opener is
currently supported only on macOS. Successful commands produce no standard
output.

```text
cargo run -p file-peeker-client -- test open ./report.pdf
cargo run -p file-peeker-client -- test open '~/reports/final.pdf' --remote example.test
```

### `test walk PATH [--remote SERVER]`

Recursively walks `PATH` on the selected host and writes each descendant path
to standard output on its own line. The root itself is omitted, directories
precede their descendants in pre-order depth-first traversal, and symbolic
links are emitted but never followed. Output follows filesystem traversal order
and is not sorted.

Relative paths preserve the requested root spelling in the output. After a
successful traversal, a debug line on standard error reports the entry count,
elapsed milliseconds, and entries per second. With `--remote SERVER`, traversal
runs on that host through the authenticated gRPC connection.

```text
cargo run -p file-peeker-client -- test walk .
cargo run -p file-peeker-client -- test walk ~/reports --remote example.test
```
