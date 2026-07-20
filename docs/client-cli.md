# Client CLI

`file-peeker-client` is a test-only command-line tool for exercising local and
remote server provisioning, startup, and protocol connections.

## `test` subcommands

All test-only operations are nested under the top-level `test` command.

### `test connect [SERVER]`

Without `SERVER`, force-installs the server from the current workspace under
`~/.file-peeker/servers/VERSION`, starts it locally, authenticates a persistent
control connection, checks it with a heartbeat, and prints the port and token as
one line of JSON.

With `SERVER`, uploads the current Git-aware project contents to
`.file-peeker/debug/repo`, force-reinstalls the server from that source, and
performs the same checks through SSH. Exiting either test sends the shutdown
request.

```text
cargo run -p file-peeker-client -- test connect
cargo run -p file-peeker-client -- test connect example.test
```

### `test install [--force] [--source PATH] [SERVER]`

Checks for the matching server executable under
`~/.file-peeker/servers/VERSION/bin/file-peeker-server` and attempts to install
it with Cargo using the version directory as `--root` when it is missing. The
resolved executable path is written to local standard output. Use `--force` to
reinstall the server even when the executable already exists.

Without `SERVER`, installation runs locally and uses the current workspace as
its source by default. In this mode, `--source` selects another local workspace.
With `SERVER`, the command uploads the current project to
`.file-peeker/debug/repo` on that host before installing over SSH; `--source`
replaces that remote workspace path. In both modes, the installer uses the
`crates/file-peeker-server` package inside the selected workspace.

```text
cargo run -p file-peeker-client -- test install
cargo run -p file-peeker-client -- test install --force
cargo run -p file-peeker-client -- test install --source /path/to/file-peeker
cargo run -p file-peeker-client -- test install example.test
cargo run -p file-peeker-client -- test install --force example.test
cargo run -p file-peeker-client -- test install --source .file-peeker/debug/repo example.test
```

### `test ssh-connection SERVER`

Connects to `SERVER`, sends `echo 'connect to SERVER'` through SSH standard
input, and writes only the last line of remote output to local standard output.

```text
cargo run -p file-peeker-client -- test ssh-connection example.test
```

### `test start-server SERVER`

Uploads the current Git-aware project contents to `.file-peeker/debug/repo` on
`SERVER`, force-reinstalls the matching server executable from that source,
starts it, and prints its port and token as one line of JSON. The test closes
the SSH input after startup, which stops the remote server before the command
exits.

```text
cargo run -p file-peeker-client -- test start-server example.test
```

### `test list PATH`

Force-installs the server from the current workspace, starts it locally, and
lists the direct children of `PATH`. Relative paths are resolved from the
client's current working directory. Each child's absolute path is written to
standard output on its own line; output order follows the server's filesystem
iteration order. After the listing completes, a debug line on standard error
reports the entry count, batch count, elapsed milliseconds, and entries per
second.

```text
cargo run -p file-peeker-client -- test list .
cargo run -p file-peeker-client -- test list /tmp/report-drafts
```
