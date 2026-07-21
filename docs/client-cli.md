# Client CLI

`file-peeker-client` is a test-only command-line tool for exercising local and
remote server provisioning, startup, and protocol connections.

## `test` subcommands

All test-only operations are nested under the top-level `test` command.

### `test connect [--force] [SERVER]`

Without `SERVER`, ensures the server is installed from Git under
`~/.file-peeker/servers/VERSION`, starts it locally, authenticates a persistent
control connection, checks it with a heartbeat, and prints the port and token as
one line of JSON. Use `--force` to reinstall the server first.

With `SERVER`, ensures the server is installed from Git and performs the same
checks through SSH. `--force` also reinstalls the remote server. Exiting either
test sends the shutdown request.

```text
cargo run -p file-peeker-client -- test connect
cargo run -p file-peeker-client -- test connect --force
cargo run -p file-peeker-client -- test connect example.test
```

### `test install [--force] [--from-source PATH] [SERVER]`

Checks for the matching server executable under
`~/.file-peeker/servers/VERSION/bin/file-peeker-server` and attempts to install
it from Git with Cargo using the version directory as `--root` when it is
missing. The resolved executable path is written to local standard output. Use
`--force` to reinstall the server even when the executable already exists.
Providing `SERVER` runs the same Git installation over SSH.

`--from-source PATH` selects an independent source-install workflow. Without
`SERVER`, Cargo installs `PATH/crates/file-peeker-server` locally. With
`SERVER`, the command uploads the Git-aware contents of local `PATH` to
`.file-peeker/debug/repo` and installs that uploaded package over SSH. This
option does not change normal connection provisioning, which remains Git-only.

```text
cargo run -p file-peeker-client -- test install
cargo run -p file-peeker-client -- test install --force
cargo run -p file-peeker-client -- test install --from-source /path/to/file-peeker
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

Ensures the server is installed from Git, starts it locally, and lists the direct
children of `PATH`. Relative paths are resolved from the server's reported
working directory, which locally inherits the client's working directory. Each
child's path is written to standard output on its own line; output order follows
the server's filesystem iteration order. `~` and `~/...` are preserved in the
output and resolved from the server's home directory. After the listing
completes, a debug line on standard error reports the entry count, elapsed
milliseconds, and entries per second.

With `--remote SERVER`, the matching server is started through SSH and the same
listing operation runs on that host. Relative paths are resolved from the
remote server's working directory.

```text
cargo run -p file-peeker-client -- test list .
cargo run -p file-peeker-client -- test list /tmp/report-drafts
cargo run -p file-peeker-client -- test list . --remote example.test
```
