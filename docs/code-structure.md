# Code Structure

This document records the intended code structure for File Peeker. It has three
parts:

1. the module structure currently adopted by the project;
2. placement and growth rules for future changes; and
3. implemented examples showing how those rules apply.

The goal is not to make every layer mirror every operation. Each layer should
own one coherent responsibility and expose the smallest useful interface to the
layer above it.

Detailed operation contracts do not live here. See the relevant document in
[Operations](README.md#add-or-change-a-feature) for caller-visible behavior,
failure policy and resource lifetime.

## Part I: Current module structure

### Responsibility boundaries

| Layer | Owns | Does not own |
| --- | --- | --- |
| `crate::fs` | Host-local filesystem behavior, transport-neutral types, streaming and cancellation semantics | Session selection, SSH, gRPC, UI behavior |
| `crate::protocol` | The generated wire contract and its transport limits, shared by both sides of the gRPC hop | Any filesystem or workflow behavior |
| `SessionBackend` | Primitive capabilities against the Session's selected host | Multi-step user workflows or operating-system UI actions |
| `Session` | Target-independent workflow entry points, backend lock/lifecycle, and selected-host/client-host phase ordering | Staging mechanics, system-command details, or protobuf |
| Client `FileService` | Client-host file preparation and operating-system workflows after selected-host data is acquired | Backend access, selected-host primitives, transport, or Session lifecycle |
| Remote backend modules | One gRPC request/response adapter per backend capability | Public workflow policy |
| `crate::server` | Listener and process lifecycle, plus RPC validation, protobuf conversion, transport batching and delegation to `fs` | Client workflows and UI state |
| `crate::tui` | Application state, input routing, rendering and task ownership | Filesystem and transport semantics |
| `crate::cli` | A test-only command surface that drives `Client` and `Session` workflows one at a time, and remote server provisioning over SSH | Behavior of its own; it composes existing workflows and prints results |
| `test_support` | Scripted Session fixtures | Production behavior; it is compiled under `cfg(test)` only |

The main boundary is:

```text
UI
   |
   v
Session workflow
   |
   v
SessionBackend capability
   |-- local  --> FsService --------------------------> host filesystem
   `-- remote --> gRPC adapter --> server op --> FsService --> host filesystem
```

`Session` and `SessionBackend` remain separate. A backend method answers “what
can be done against the selected host?”, while a Session operation answers
“what workflow does the caller want?”. Simple workflows may delegate once;
more involved workflows may compose several backend capabilities plus
client-host behavior.

### One package, three binaries

Status: implemented.

Everything lives in a single package rooted at the repository. The three
binaries under `src/bin/` are shims: each one calls the `run` entry point of its
feature-gated module, so all logic stays in the library where `cfg(test)` reaches
it.

```text
Cargo.toml   build.rs   proto/file_peeker.proto
src/
├── lib.rs                  module declarations and the crate-internal facade
├── fs/                     host-local filesystem behavior
├── protocol.rs             wire contract, including the generated `v1` module
├── client.rs  session/     the Client and Session layer
├── test_support.rs         cfg(test) fixtures
├── server/                 the file-peeker-server binary's logic
├── cli/                    the file-peeker-client binary's logic
├── tui/                    the file-peeker binary's logic
└── bin/
    ├── file-peeker.rs          -> tui::run()
    ├── file-peeker-client.rs   -> cli::run()
    └── file-peeker-server.rs   -> server::run()
```

### Dependency direction

Four crates used to make the layering a compile error. One package does not, so
the rule is now written down and only partly enforced. A module may name:

| Module | May name |
| --- | --- |
| `fs` | nothing else in the crate |
| `protocol` | nothing else in the crate |
| `session`, `client` | `fs`, `protocol` |
| `server` | `fs`, `protocol` — never `session` or `client` |
| `tui`, `cli` | the crate-root facade (`Client`, `Session`, the entry types) |

`fs` and `protocol` being unreachable from `tui` is convention only. The rest is
enforced: `session`, `client`, and the facade are gated on the `client` feature,
which `server` does not enable, so a `--features server` build cannot compile a
reference to them.

Visibility carries the rest. Only `tui::run`, `cli::run`, and `server::run` are
`pub`; everything else is `pub(crate)` or private, and the `unreachable_pub`
lint keeps it that way.

### The filesystem layer

```text
src/fs/
├── mod.rs
├── error.rs
├── fs_service.rs
├── resolve_path.rs
├── read.rs
├── write.rs
└── directory/
    ├── mod.rs
    ├── entry.rs
    ├── list.rs
    └── walk.rs
```

Spell paths into this module `crate::fs::…`. A bare `fs::…` would collide with
`std::fs` at any call site that imports it.

- `mod.rs` is a thin facade and re-exports the types other layers use.
- `FsService` is the cohesive entry point for filesystem behavior and shared
  cancellation.
- `resolve_path.rs`, `read.rs`, and `write.rs` hold focused operations that do not yet form
  larger domain families.
- `directory/` owns shared entry inspection, one-level listing, and recursive
  traversal types and behavior.
- Tests stay beside the behavior they verify.

The asymmetry is intentional. A `file/` directory should not be created merely
to wrap the single `read.rs` operation. It becomes useful when several
file-oriented operations need shared types or policy.

### Client and Session

Status: implemented.

```text
src/session/
├── mod.rs
├── path.rs
├── directory/
│   ├── mod.rs
│   ├── list.rs
│   └── walk.rs
├── file/
│   ├── mod.rs
│   └── copy.rs
├── backend/
│   ├── mod.rs
│   ├── local.rs
│   ├── error.rs
│   ├── connection/
│   │   ├── mod.rs
│   │   └── remote.rs
│   └── remote/
│       ├── mod.rs
│       ├── entry.rs
│       ├── error.rs
│       ├── resolve_path.rs
│       ├── list_dir.rs
│       ├── read_file.rs
│       ├── write_file.rs
│       └── walk_dir.rs
```

`src/client.rs` and `src/test_support.rs` sit beside this tree at the crate root.

- `session/mod.rs` owns Session construction, lifecycle and the private backend
  handle.
- `path.rs` contains Rust-native Session path workflows.
- `directory/` contains transport-neutral directory result types and native
  Session directory workflows.
- `file/open.rs` contains the Rust-native Session entry point and selected-host
  coordination.
- `file/copy.rs` owns the destination-oriented Session-to-Session streamed copy
  workflow.
- `file/service.rs` defines the private client `FileService` facade for
  client-host file preparation and operating-system workflows.
- `file/stage.rs` owns platform cache-root selection, lazy private-directory
  creation, safe streaming publication, and incomplete-file cleanup, while
  `file/opener.rs` owns the substitutable operating-system/test seam. They
  remain separate because cache policy and process launching are distinct
  responsibilities.
- `backend/mod.rs` defines the private `SessionBackend` capability seam.
- `backend/local.rs` adapts `FsService`; `backend/remote/` contains exact gRPC
  adapters for the same primitive capabilities. `remote/write_file.rs` owns
  client-streaming request framing and response validation.
- `backend/connection/` owns remote connection and server-process lifecycle,
  not filesystem operations.
- `test_support.rs` provides the scripted `Session` fixtures every test module
  shares. A `#[cfg(test)] mod tests` in one file cannot see helpers declared in
  another, so the fixtures need a module of their own; `cfg(test)` then keeps them
  structurally unreachable from any release build.

`read_file` remains a private backend primitive rather than a caller-facing
native read operation. The Session copy workflow consumes it directly, streaming
the source Session's bytes into the destination Session's write.

### Server

Status: implemented.

```text
src/server/
├── mod.rs
├── serve.rs
└── ops/
    ├── mod.rs
    ├── entry.rs
    ├── status.rs
    ├── list.rs
    ├── read.rs
    ├── write.rs
    └── walk.rs
```

- `mod.rs` owns command-line parsing, version reporting, and `run`, the single
  `pub` item the `file-peeker-server` shim calls.
- `serve.rs` owns listener setup, authentication, health reporting, startup,
  shutdown and the `serve` lifecycle.
- `ops/mod.rs` is the tonic service adapter and keeps trivial delegation, such
  as path resolution, inline.
- `ops/list.rs`, `ops/read.rs`, `ops/write.rs`, and `ops/walk.rs` own behavior-heavy streaming
  conversion and batching; `ops/entry.rs` shares listing-entry conversion.
- `ops/status.rs` owns the status-to-gRPC error mapping shared by server
  operations.

There is no server backend trait. The server has one real filesystem
implementation, `FsService`; adding an interface there now would create a seam
without variation.

### TUI

Status: implemented.

```text
src/tui/
├── mod.rs
├── browser_context.rs
└── app/
    ├── mod.rs
    ├── session.rs
    └── view.rs
```

- `mod.rs` retains command-line parsing, terminal setup/restore, the event loop,
  direct key-to-command mapping, and `run`.
- `app/mod.rs` owns `App`, ordered Session and tab records, lifecycle event/task
  routing, explicit UI modes, and application commands.
- `app/session.rs` narrowly owns TUI Session identity, derived labels, target
  presentation, and Starting/Ready/Failed/Closing state.
- `app/view.rs` renders application state without owning async tasks or mutating
  browser state.
- `browser_context.rs` is the deep browsing module: selected listing source,
  current path, entries, selection, listing generation, task cancellation,
  stale-result rejection and terminal listing state.

This split keeps high-level orchestration visible without fragmenting
`BrowserContext` into state, event and task files that would need to understand
the same invariants.

### Test CLI

Status: implemented.

```text
src/cli/
├── mod.rs
├── command.rs
├── connect.rs
├── install.rs
├── list.rs
├── open.rs
├── ssh_connection.rs
├── start_server.rs
├── walk.rs
└── install-server-from-source.sh
```

- `command.rs` owns the clap tree, tracing setup, and `run`.
- One module per subcommand, each driving a single `Client` or `Session`
  workflow and printing what it observed. These modules add no behavior of their
  own; see [Client CLI](client-cli.md).
- `install.rs` and its bundled shell script own remote provisioning from an
  uploaded source tree. The script is `include_str!`'d, so it has to sit beside
  the module rather than in `scripts/`.

### Binary targets and features

`default` enables all three binaries, so `cargo test` and
`cargo clippy --all-targets` cover everything without extra flags.

| Feature | Adds | Gated dependencies |
| --- | --- | --- |
| `client` | `client.rs`, `session/`, and the crate-root facade | async-trait, dirs, hyper-util, serde, tower, tracing, uuid |
| `cli` | `crate::cli` and the `file-peeker-client` binary; implies `client` | tracing-subscriber |
| `tui` | `crate::tui` and the `file-peeker` binary; implies `client` | crossterm, ratatui |
| `server` | `crate::server` and the `file-peeker-server` binary | none |

Each `[[bin]]` declares its feature as `required-features`. This is what lets
remote provisioning build a server without the front ends:

```sh
cargo install --no-default-features --features server --bin file-peeker-server …
```

`fs`, `protocol`, and the proto codegen in `build.rs` stay unconditional. Both
sides of the wire use `protocol::v1`, and the generated code names prost and
tonic, so those cannot be gated behind `server`.

One hazard comes with `required-features`: `build`, `check`, `test`, and
`clippy --all-targets` **silently skip** a target whose features are unmet unless
it is named explicitly. `cargo clippy --all-targets --no-default-features` lints
zero binaries and still reports success. `make check` therefore runs a
`slim-server` step that names the binary, so a feature mistake fails the build
instead of vanishing.

## Part II: Placement and growth rules

### Placement rule for a new operation

Before adding a file, identify the operation's semantic owner:

1. Put host-local filesystem mechanics in `crate::fs`.
2. Put a primitive selected-host capability in `SessionBackend`.
3. Implement local and remote adapters only when that capability must work on
   both targets.
4. Put public multi-step ordering and lifecycle coordination in `Session`.
5. Put post-backend client-host file preparation and OS actions in
   `FileService`.
6. Put protobuf conversion and wire batching in the remote adapter and server
   `ops`.
7. Add a UI adapter only when that boundary has a real consumer.

A public Session workflow does not require an identically named core, backend
and RPC operation. Layers should mirror semantics only where the semantics
are actually shared.

### Growth rules

- Prefer a new function when behavior has different semantics. Do not hide
  recursive traversal behind `list_dir(recursive: bool)`.
- Add an options type only when at least one real policy choice exists. Avoid
  empty or speculative option structs.
- Create a domain directory when multiple operations share vocabulary, types
  or invariants. Uneven module depth is acceptable.
- Extract shared batching, conversion or task helpers after a second caller
  demonstrates the duplication and its common invariant.
- Add a trait only when there are real implementations to vary, or a boundary
  must be substituted in tests.
- Keep streaming pull-based and preserve partial results followed by a terminal
  error unless a feature explicitly requires different semantics.
- Avoid adding UI surface area until a concrete consumer needs it.
- Prefer a `cfg(test)` fixture in `test_support` over a trait introduced only
  to be faked.
- Keep a new binary's logic in a feature-gated module with a single `run` entry
  point. Do not put logic in `src/bin/`: a binary target cannot see the
  library's `cfg(test)` items.

### Server growth

- Keep small validation/delegation methods in `ops/mod.rs`.
- Give each behavior-heavy streaming RPC its own `ops/<operation>.rs` module.
- Keep transport limits and protobuf conversion on the server side; `fs`
  streams should not inherit gRPC message-size policy.
- Introduce shared operation infrastructure only after at least two operations
  need the same invariant.
- Do not add a server repository/backend abstraction while `fs::FsService` remains
  the sole implementation.

### TUI growth

- Keep the current pure mode-aware key mapping in `tui/mod.rs`; add
  `app/input.rs` only if configurable bindings or a command palette justify it.
- Add a `locations/` or sidebar domain only when it owns state and actions, not
  merely because a second panel is rendered.
- Keep `BrowserContext` cohesive until recursive traversal, search or another
  task family proves a smaller reusable task abstraction.
- UI modules may consume `walk_dir` or `open_file`, but they should not recreate
  traversal, caching or transport policy.

### Checklist for adding a feature

1. Define the caller-visible semantics, especially streaming, partial-result,
   cancellation and error behavior.
2. Place the behavior at its semantic owner rather than copying the operation
   across all layers.
3. Add `fs` types and facade re-exports only when the behavior is
   transport-neutral.
4. Extend `SessionBackend` only for a selected-host primitive.
5. Add local and remote implementations, plus protocol and server work, only
   where remote execution requires them.
6. Add Session composition for caller workflows.
7. Place client-host file actions behind `FileService` only after Session has
   released its backend lock; selected-host primitives still follow the
   core/backend placement rules.
8. Add UI adapters only for real consumers.
9. Keep tests beside the layer-specific invariant they verify.
10. Update this document when the new feature changes a responsibility boundary.

## Part III: Implemented placement examples

### Recursive directory traversal

Recursive traversal is a separate `walk_dir` capability rather than a mode of
`list_dir`. Listing one directory and traversing a tree have different cost,
ordering, cancellation and error semantics.

Implemented structure:

```text
src/fs/directory/
├── mod.rs
├── entry.rs
├── list.rs
└── walk.rs

src/session/
├── directory/
│   ├── mod.rs
│   ├── list.rs
│   └── walk.rs
└── backend/
    ├── mod.rs
    ├── local.rs
    └── remote/
        ├── mod.rs
        ├── entry.rs
        ├── list_dir.rs
        └── walk_dir.rs

src/server/ops/
├── mod.rs
├── entry.rs
├── list.rs
└── walk.rs
```

`fs` list and walk share entry inspection because classification is a
host-local filesystem concern. The client exposes separate directory workflows
and backend capabilities. The server shares entry conversion and transport
limits while retaining distinct List and Walk protobuf adapters. This keeps
different traversal semantics visible without duplicating shared mechanics.

Caller-visible behavior is documented in [Client API](api.md); remote batching
and validation are owned by [Remote Protocol](protocol.md).
