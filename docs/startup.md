# Startup routine

Local and remote session startup implement the protocol-v1 connection and
lifecycle model below.

Local and remote targets are designed to share one lifecycle model:
one loopback TCP server per `Session`, one memory-only token, one authenticated
connection per request, heartbeat health checks, and stdin-owned lifetime.

```text
Local:  Session -> TCP 127.0.0.1:PORT -> server
Remote: Session -> SOCKS 127.0.0.1:PROXY -> one SSH transport
               -> remote TCP 127.0.0.1:PORT -> server
```

## Local startup

`Client::start_session(SessionTarget::Local)` starts a managed local server,
retains its Session by UUID, and returns that UUID.

1. Ensure the matching executable exists below
   `~/.file-peeker/servers/VERSION`. The public client and test CLI reuse or
   install that version; `test connect --force` explicitly reinstalls it.
2. Spawn the resolved executable as `file-peeker-server serve` with piped
   stdin, stdout, and stderr.
3. Parse the prefixed startup JSON from stdout.
4. Open a direct TCP connection, authenticate, and complete a heartbeat.
5. Return a lifecycle owner retaining the child, stdin lease, endpoint, token, heartbeat,
   connection limit, diagnostics, and bounded shutdown.

The server binds only `127.0.0.1:0`; host and port are not configurable. All
diagnostics use stderr. Any later stdout data is a fatal launcher violation.

## Remote startup

1. Validate the SSH destination and create an owner-only local runtime/log
   directory with a short OpenSSH control-socket path.
2. Select an ephemeral local SOCKS port and start a foreground OpenSSH master:

   ```text
   ssh -M -S <control> -o ControlPersist=no -N -T \
       -D 127.0.0.1:<proxy-port> <destination>
   ```

3. Wait for the master and authentication. Installation/version checks reuse
   the authenticated transport through multiplexed, non-interactive helpers.
4. Launch the matching remote server through another multiplexed session
   channel. The helper's stdin is passed directly to the server as its lifetime
   lease; stdout carries the two startup records.
5. Open a SOCKS5 connection to remote `127.0.0.1:<server-port>`, authenticate
   with the token, and complete a heartbeat.
6. Retain the same public `Session` type used locally and return its UUID.

There is one authenticated SSH network transport. Helper `ssh` processes only
request additional channels through its control socket. Every request creates a
new local TCP socket, SOCKS handshake, `direct-tcpip` channel, application
handshake, and operation protocol state.

OpenSSH does not accept port zero for `-D`. The launcher discovers a candidate
port by briefly binding loopback port zero and retries startup with a new port
up to three times if OpenSSH loses the bind race.

## Runtime behavior

A heartbeat runs after 15 seconds without successful protocol activity and has
a five-second connection/authentication timeout. Any heartbeat, operation
transport, authentication, or protocol failure immediately and permanently
closes the session. Ordinary filesystem errors do not. Callers create a new
session explicitly; automatic reconnection is intentionally absent.

Each session allows 64 simultaneous operation connections, with a separate
heartbeat allowance. The server defensively caps active connections at 128.

`Client.close_session(id)` removes the retained Session and performs its
graceful shutdown. Explicit `Session.close()` remains immediate and idempotent
but does not remove it from Client: it stops heartbeat,
closes the stdin lease, cancels active operations, waits up to the bounded
shutdown deadline, terminates the SSH master when present, and removes owned
local runtime files. Dropping the last session/listing reference initiates the
same cleanup.

## Modules

- `server/mod.rs`: shared connection runtime, heartbeat, limits, fatal state,
  and shutdown facade.
- `server/protocol.rs`: startup parsing, direct/SOCKS connections, v2
  authentication, heartbeat, and NDJSON I/O.
- `server/local.rs`: local process launch, rollback, and supervision.
- `server/remote.rs`: SOCKS master, installation, remote launch, rollback, and
  supervision.
- `server/ssh.rs`: OpenSSH validation, multiplexing, readiness, and exit.
- `server/session_directory.rs`: owner-only per-session directories, log files,
  rotation, and cleanup.
