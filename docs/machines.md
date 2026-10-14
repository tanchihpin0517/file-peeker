# Machines

How v2 reaches a server on another machine: saving it, installing `fpk` on it,
the bridge the client protocol runs over, and how the terminal UI lists every
machine. [v2 architecture](design/v2-architecture.md) is the proposal; this is
what is built.

## Status

Implemented in [`client::machines`](../src/client/machines.rs),
[`client::ssh`](../src/client/ssh.rs), [`client::remote`](../src/client/remote.rs),
[`cli::machine`](../src/cli/machine.rs), [`cli::host`](../src/cli/host.rs) and
the terminal UI's [`connection`](../src/tui/connection.rs). Remote machines show
their servers' tasks; nothing browses yet.

## The shape

As in herdr, every machine runs its own server and the client aggregates them;
servers never talk to each other. A remote server is reached by running
`fpk bridge` on the host over SSH and speaking the client protocol over that
process's standard input and output.

![The terminal UI reaching the local server on its socket, and each host through ssh and the host's fpk bridge](assets/machines-shape.svg)

No port listens anywhere and no socket path is forwarded.

## Commands

| Command | Does |
| --- | --- |
| `fpk machine add <label> <ssh-target>` | Probes the host, installs `fpk` there if it needs this client's build and you agree, starts its server, and saves it. On a saved label, probes again and offers to update |
| `fpk machine add … --yes` | The same, agreeing to every question: install, update, restart |
| `fpk machine list` | The saved machines |
| `fpk machine remove <label>` | Forgets one. Its server and tasks keep running |
| `fpk machine login <label>` | Opens the shared SSH connection interactively, for a host whose login needs answering |
| `fpk --machine <label> <command>` | Runs `status`, `server start`, `server stop` or `task …` on the host, passing its output through and exiting as it exits |

`--machine` may come anywhere on the line. The terminal UI refuses it, because
it already lists every machine.

## Files

Every machine keeps its own files, even when machines share a home directory,
as a cluster's nodes do: nothing below is shared between two machines. A
release build uses `~/.file-peeker/` and `/tmp/fpk-<uid>/`, and a dev build
`~/.file-peeker-dev/` and `/tmp/fpk-dev-<uid>/`, on the client and on every
host. The two never share any of it, so `cargo run` from a checkout never meets
the server a release install is using, here or on a host. The build profile
decides at compile time, as it decides how a build installs. Below,
`~/.file-peeker/` stands for whichever of the two the build uses.

| Path | Holds |
| --- | --- |
| `~/.file-peeker/hosts/<machine-id>/` | This machine's directory, mode `0700`: everything below that lasts |
| `~/.file-peeker/by-name/<hostname>` | A link to it, for a person reading the tree; nothing follows it |
| `/tmp/fpk-<uid>/` | This machine's connections, mode `0700`: the sockets and `ssh/` |

The machine ID is Linux's `/etc/machine-id`, or `/var/lib/dbus/machine-id`, or
macOS's `IOPlatformUUID`, as 32 lowercase hex digits. A machine without one
refuses to start. Both directories are refused unless this user owns them and
no one else may open them.

On the client, in its machine's directory:

| File | Holds |
| --- | --- |
| `machines.toml` | Saved machines: an ID, the label, the SSH target, `enabled` |
| `config.toml` | `[remote] manage_ssh_config`, default `true` |

and in `/tmp/fpk-<uid>/ssh/`, mode `0700`, the shared connections' control
sockets.

```toml
# machines.toml
[[machine]]
id = "m-7f3a91c2"
label = "workbox"
ssh = "me@workbox"
enabled = true
```

A label is one word and cannot be `local`. A saved label cannot be pointed at
another host; remove it first. `machines.toml` is written beside itself and
renamed over, so a crash leaves the old catalog whole. Another machine sharing
this home has a catalog of its own.

On a host, in its machine's directory:

| Path | Holds |
| --- | --- |
| `bin/fpk` | The installed binary — never on the host's `PATH`, never anywhere else |
| `bin/fpk.source` | A dev install's source hash |
| `src/`, `build/`, `cargo/` | A dev install's uploaded source, kept target directory, and `cargo install` root |

Two nodes sharing a home therefore install and build separately: they need not
share an architecture or a C library. Nothing from the client's files is copied
to a host.

## SSH

Every remote operation is one `ssh <options> -T <target> <command>`.

| Option | When | Why |
| --- | --- | --- |
| `ConnectTimeout=10` | Always | A dead host fails rather than hangs |
| `ControlMaster=auto`, `ControlPath=/tmp/fpk-<uid>/ssh/%C`, `ControlPersist=600` | `manage_ssh_config = true` | Setup, reconnects and `--machine` share one authenticated connection, as herdr's do |
| `BatchMode=yes` | Background reconnects | A reconnect never prompts; it fails instead |

With `manage_ssh_config = false` the client adds no control options, and your own
SSH configuration decides — for a host whose login needs a ControlMaster of your
own making.

### Path lengths

A unix socket's path can be at most 103 bytes on macOS and 107 on Linux — the
size of `sockaddr_un`'s `sun_path`, less its terminating NUL. As herdr does, `fpk`
checks the two socket paths it depends on itself and says what to do, rather
than leaving `bind` or `ssh` to fail with a message about something else:

| Socket | Checked by | Its path | Fix |
| --- | --- | --- | --- |
| `api.sock`, `client.sock` | The server before it binds, and a client before it starts one | `/tmp/fpk-<uid>/` plus the name | A shorter `FILE_PEEKER_HOME` |
| A shared SSH connection's control socket | Every SSH command, when `manage_ssh_config` is on | `/tmp/fpk-<uid>/ssh/`, then `/`, `%C`'s 40 hex characters, and the 17 OpenSSH adds while it binds a new master | `manage_ssh_config = false`, or a shorter `FILE_PEEKER_HOME` |

Under `/tmp` both fit with room to spare — the control socket, the longer, is
79 bytes for a dev build — which is why the sockets are not in the home
directory with everything else. Only a long `FILE_PEEKER_HOME` can exceed them.

Setup, `machine login` and `--machine` may prompt: a password or a
keyboard-interactive login is answered on your terminal. The terminal UI's
reconnects may not; a host that wants an answer shows as needing a login until
`fpk machine login` opens the connection.

## Probing

Every command the client runs on a host goes through `sh -c`, whatever the
login shell, and starts with the same snippet: it computes the host's machine ID
and sets `FPK_HOST_DIR` to `~/.file-peeker/hosts/<machine-id>`, or to
`FILE_PEEKER_HOME` when that is set there. It computes it on every command,
since one SSH target may reach a different node each time, and a test holds it
to the Rust that resolves the same directory. A host with no machine ID fails
there, saying so.

Before anything else, the client runs one command on the host:

```sh
f="$FPK_HOST_DIR/bin/fpk"; if [ -x "$f" ]; then exec "$f" probe; else echo '{"installed":false}'; fi
```

`fpk probe` prints one line of JSON:

```json
{"installed":true,"version":"0.0.0","client_protocol":3,"build":"dev","source_hash":"fa8c0ad8…","server_running":true}
```

| SSH ended | Read as |
| --- | --- |
| Success, with a probe line | What is installed, and whether its server runs |
| Exit 255 mentioning `Permission denied`, host key verification, a password or a keyboard-interactive login | **Needs a login** |
| Anything else | **Unreachable**, for now |

## Installing

What a client installs is decided by its own build profile, which Rust fixes at
compile time ([`build_info`](../src/build_info.rs)):

| | A release build | A dev build |
| --- | --- | --- |
| Is | `cargo build --release`, and every published binary | `cargo build`, `cargo run` |
| Wants on the host | The release of **its own version** | A dev build of **the source it was compiled from** |
| Installs by | [`install-release.sh`](../src/client/install-release.sh) on the host | Uploading the source, then [`install-dev.sh`](../src/client/install-dev.sh) on the host |
| The host needs | Outbound HTTPS, `curl` or `wget` | A Rust toolchain |
| Reinstalls when | The host's build or version differs | The host's build or source hash differs |

Either way the host's client protocol must also match. `fpk build-info`, hidden,
prints what this client wants.

**A release install** picks `fpk-<linux|macos>-<x86_64|aarch64>` from `uname`,
downloads it and `SHA256SUMS` from
`github.com/tanchihpin0517/file-peeker/releases/download/v<version>/`, checks the
one against the other, and moves it into place. A version with no release — any
build between releases — cannot be installed this way, and says so.

**A dev install** uploads, as a tar stream over SSH into the host's `src/`,
every file `git ls-files --cached --others --exclude-standard` lists under the
directory the binary was compiled in, except `old/`: tracked and untracked but
not ignored, so uncommitted edits travel too. The client hashes those files —
paths, lengths and contents, in order — and the host runs
`cargo install --debug --locked` into a kept target directory, so a rebuild is
incremental, then records the hash beside the binary. A first build takes about
a minute; editing the source and running `machine add` again is how a host
catches up.

Setup asks before installing, and asks **again** before restarting a server
already running the old build, because restarting cancels its tasks, as herdr
asks before replacing a server. Without a terminal to ask on, the answer is no
unless `--yes` was given. Declined or failed, the machine is not saved.

## The terminal UI

The sidebar lists `local` first, then each enabled saved machine in the order it
was added. Each has a connection task of its own with its own backoff, so a slow
or lost host holds up nothing else and `local` opens at once.

A remote connection attempt probes before it bridges, so each way of failing
says what it is:

| The probe found | The machine shows | Then |
| --- | --- | --- |
| Needs a login | `attention`: "wants you to sign in", and `fpk machine login` | Checks again on the backoff |
| No `fpk` | `attention`: "not installed", and the `machine add` to run | Checks again on the backoff |
| Another client protocol | `attention`: what it has, and the `machine add` that updates it | Checks again on the backoff |
| Unreachable | `retry`, with SSH's reason | Reconnects on the backoff |
| The server not running | `retry`: "its server is not running" | Reconnects on the backoff; `s` starts it |
| A running server | `attached`, after the bridge's handshake | Its tasks in the lower half |

Selecting a machine with `j`/`k`, the arrow keys, a click or the wheel shows
its tasks; clicking a key hint presses its key. `s` on a disconnected machine
starts its server — on a remote one, `fpk server start` over SSH. Nothing in the
terminal UI ever installs anything, and the bridge itself never starts a server:
a reconnect must not bring back a server someone stopped.

## Releases

[`release.yml`](../.github/workflows/release.yml) runs on a `v*` tag:

1. The tag must equal `v` plus `Cargo.toml`'s version.
2. Four builds: Linux x86_64 and aarch64 as static musl binaries through
   `cargo zigbuild`, and macOS x86_64 and aarch64.
3. One release with the four `fpk-<os>-<arch>` binaries and `SHA256SUMS`. A tag
   with a pre-release part, such as `v0.1.0-alpha.1`, publishes a pre-release.

## Testing without a host

A unix socket's path is limited to about a hundred bytes — 104 on macOS — so
the tests root their homes under `/tmp` rather than the system's much deeper
per-user temporary directory; a host's socket sits three directories below.

`FILE_PEEKER_SSH` replaces the `ssh` program. The end-to-end tests in
[`tests/machines.rs`](../tests/machines.rs) point it at a stand-in that skips
SSH's options and runs the remote command locally under a second home
directory, so the probe, setup, starting the host's server and `--machine` all
run the real binary on both sides.

## Not built yet

- Closing an idle bridge after a minute.
- A second server per machine for a named session, as herdr runs one per session.
- herdr's version negotiation: a host whose protocol differs is updated, not
  negotiated with.
- Enabling and disabling a saved machine from the command line; `enabled` is
  only edited by hand.
