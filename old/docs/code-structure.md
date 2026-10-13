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
| `crate::fs` | Host-local filesystem behavior, transport-neutral types, streaming and cancellation semantics | Connection selection, SSH, gRPC, UI behavior |
| `crate::protocol` | The generated wire contract and its transport limits, shared by both sides of the gRPC hop | Any filesystem or workflow behavior |
| `Client` | Target-independent workflow entry points, the connection registry and its per-connection locks, and cross-connection phase ordering | Transport details, server lifecycle, or protobuf |
| `client/connection` | Where a connection's server runs, how a stream reaches it, and its startup/shutdown | Filesystem operations or workflow policy |
| `client/rpc` modules | One gRPC request/response adapter per Client operation | Public workflow policy |
| `crate::server` | Listener and serving lifecycle, plus RPC validation, protobuf conversion, transport batching and delegation to `fs` | Client workflows and UI state |
| `crate::tui` | Application state, input routing, rendering and task ownership | Filesystem and transport semantics |
| `crate::gui` | The same, for the desktop front end: an egui/eframe workspace whose layout `egui_dock` owns, and a Tokio bridge that turns listings into per-frame updates | Filesystem and transport semantics; it holds no behavior the TUI does not also need |
| `crate::cli` | A test-only command surface that drives `Client` workflows one at a time, and remote server provisioning over SSH | Behavior of its own; it composes existing workflows and prints results |
| `test_support` | Scripted server fixtures | Production behavior; it is compiled under `cfg(test)` only |

The main boundary is:

```text
UI
   |
   v
Client workflow
   |
   v
rpc adapter per operation
   |
   v
ServerConnection
   |-- embedded server ------> client-host filesystem
   `-- SSH -> remote server -> server-host filesystem
```

There is one backend. `ServerConnection` is where the two targets diverge, and
they diverge in exactly two values: which process was started, and which
connector reaches its loopback port.

`Client` and the `rpc` modules remain separate. An `rpc` function answers
“what does one request against the selected host look like?”, while a Client
operation answers “what workflow does the caller want?”. Simple workflows may
delegate once; more involved workflows, such as a cross-connection copy, compose
several requests and order the locks between them.

### One package, four binaries

Status: implemented.

Everything lives in a single package rooted at `old/`, a workspace of its own.
The repository root holds the [v2 rewrite](../../docs/design/v2-architecture.md)
instead: it names nothing here, and nothing here names it. The four binaries under `src/bin/` are shims: each one calls the `run` entry point of its
feature-gated module, so all logic stays in the library where `cfg(test)` reaches
it.

```text
Cargo.toml   build.rs   proto/file_peeker.proto
src/
├── lib.rs                  module declarations and the crate-internal facade
├── fs/                     host-local filesystem behavior
├── host_path.rs            selected-host names and paths, escaped so a `String` holds them
├── protocol.rs             wire contract, including the generated `v1` module
├── client/                 the Client layer: connections, operations, transport
├── browser/                the headless model both front ends browse with
├── test_support.rs         cfg(test) fixtures
├── server/                 the file-peeker-server binary's logic
├── cli/                    the file-peeker-client binary's logic
├── tui/                    the file-peeker binary's logic
├── gui/                    the file-peeker-gui binary's logic
└── bin/
    ├── file-peeker.rs          -> tui::run()
    ├── file-peeker-gui.rs      -> gui::run()
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
| `client` | `fs`, `protocol`, `server` |
| `server` | `fs`, `protocol` — never `client` |
| `browser` | the crate-root entry types — never a front end, a client or a runtime |
| `tui`, `gui`, `cli` | the crate-root facade (`Client`, the entry types), and `browser` |

`client` naming `server` is the point: a local connection embeds one. The
reverse stays forbidden and stays enforced. `client` and the facade are gated on
the `client` feature; `server-core` — which carries the gRPC service and its
listener — is enabled by both `client` and `server`, but `server` does not enable
`client`, so a `--features server` build still cannot compile a reference to
them. `make slim-server` is what actually runs that build, because a target
whose `required-features` are unmet is skipped in silence.

`fs` and `protocol` being unreachable from a front end is convention only.

Visibility carries the rest. Only `tui::run`, `gui::run`, `cli::run`, and
`server::run` are `pub`; everything else is `pub(crate)` or private, and the
`unreachable_pub` lint keeps it that way.

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

### Client

Status: implemented.

```text
src/client/
├── mod.rs
├── path.rs
├── directory/
│   ├── mod.rs
│   ├── list.rs
│   └── walk.rs
├── file/
│   ├── mod.rs
│   └── copy.rs
├── connection/
│   ├── mod.rs
│   ├── embedded.rs
│   ├── ssh.rs
│   └── ensure-server.sh
└── rpc/
    ├── mod.rs
    ├── entry.rs
    ├── error.rs
    ├── resolve_path.rs
    ├── list_dir.rs
    ├── read_file.rs
    ├── write_file.rs
    └── walk_dir.rs
```

`src/test_support.rs` sits beside this tree at the crate root.

- `client/mod.rs` owns the connection registry, `ConnectionId`, connection
  lifecycle and the guarded connection accessor.
- `path.rs` contains Rust-native Client path workflows.
- `directory/` contains transport-neutral directory result types and native
  Client directory workflows.
- `file/copy.rs` owns the destination-oriented connection-to-connection streamed
  copy workflow.
- `rpc/` is the calling side of the protocol: one module per operation, each
  building a request and validating the response, plus the stream and write
  types they share. `rpc/write_file.rs` owns client-streaming request framing.
  `crate::server` is the handling side; the two never name each other.
- `connection/` owns server lifecycle and transport, not filesystem
  operations. `connection/mod.rs` holds everything both targets share —
  `ServerConnection`, channel construction, the bearer token, the health check —
  while `embedded.rs` and `ssh.rs` hold only what differs: how the server is
  started, how a stream reaches it, and how it is stopped.
- `test_support.rs` provides the scripted server fixtures every test module
  shares. A `#[cfg(test)] mod tests` in one file cannot see helpers declared in
  another, so the fixtures need a module of their own; `cfg(test)` then keeps them
  structurally unreachable from any release build.

`read_file` remains a private primitive rather than a caller-facing native read
operation. The copy workflow consumes it directly, streaming the source
connection's bytes into the destination connection's write.

### Server

Status: implemented.

```text
src/server/
├── mod.rs
├── command.rs
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

- `mod.rs` re-exports `run` for the `file-peeker-server` shim and `start` for a
  client embedding its own server.
- `serve.rs` owns listener setup, authentication, health reporting, and the
  `ServerHandle` startup/shutdown lifecycle. It knows nothing about processes,
  so the same code serves the binary and an embedded server.
- `command.rs` is the binary's half: command-line parsing, version reporting,
  the startup line on stdout, and the stdin lifetime lease. It is gated on the
  `server` feature, so a client build compiles the service without it.
- `ops/mod.rs` is the tonic service adapter and keeps trivial delegation, such
  as path resolution, inline.
- `ops/list.rs`, `ops/read.rs`, `ops/write.rs`, and `ops/walk.rs` own behavior-heavy streaming
  conversion and batching; `ops/entry.rs` shares listing-entry conversion.
- `ops/status.rs` owns the status-to-gRPC error mapping shared by server
  operations.

There is no server backend trait. The server has one real filesystem
implementation, `FsService`; adding an interface there now would create a seam
without variation. The same rule now applies on the client side, which is why
the client no longer has one either. A backend trait would need real
implementations to vary, and the variation that once justified two of them moved
into `ServerConnection`, where it is two enum variants rather than two
adapters.

There is no `Session` type for the same reason. Once the backend trait was gone,
a Session was a UUID plus one connection slot — a wrapper with nothing of its
own to own, existing only so the registry had something to store. Its lifecycle
and its operations belong to `Client`, and callers name a connection with a
`ConnectionId` rather than holding a handle. A type earns its place by owning an
invariant; that one owned none.

### Browser model

Status: implemented.

```text
src/browser/
├── mod.rs
├── tree.rs
└── window.rs
```

The headless model of one browsed root, driven by both front ends. It knows how
to place a listing that arrives in groups and what a filter leaves on screen; it
knows nothing about frames, terminals or servers.

- `tree.rs` owns `Tree`: rows in pre-order, the in-flight expansions filling
  them, and the selection index. The owner starts a listing, feeds each group in
  with `extend_root`/`extend_child`, and closes it with the matching `finish`.
  `collapse` hands back the requests it dropped, so whoever holds the tasks can
  abort them.
- `window.rs` owns `VisibleRows`: the matches a query leaves on screen plus the
  ancestors that place them. Filtering never removes rows, because every
  insertion addresses rows by index.
- `mod.rs` holds `Row`, `Expansion` and `Status`.

The seam with the front ends is the viewport. A terminal owns a scroll offset and
a height it only learns at render time; a desktop scroll area owns its own
virtualisation and asks for rows by position. Neither belongs to the model, so
both ask it the same two questions — how many rows are visible, and which row
sits at a position.

`Tree` takes the query and the scope as arguments rather than storing them. The
TUI's prompt is a keystroke-at-a-time state machine with directory descents; the
GUI's is a text field. Only the resulting query is common, so only the query
crosses down.

### TUI

Status: implemented.

```text
src/tui/
├── mod.rs
├── browser_context.rs
└── app/
    ├── mod.rs
    ├── connection.rs
    └── view.rs
```

- `mod.rs` retains command-line parsing, terminal setup/restore, the event loop,
  direct key-to-command mapping, and `run`.
- `app/mod.rs` owns `App`, ordered connection and tab records, lifecycle
  event/task routing, explicit UI modes, and application commands.
- `app/connection.rs` narrowly owns TUI connection-row identity, derived labels,
  target presentation, and Starting/Ready/Failed/Closing state. Its
  `ConnectionRecordId` stays distinct from the Client's `ConnectionId`: a row
  exists in `Starting` before a connection does, and outlives a failed open.
- `app/view.rs` renders application state without owning async tasks or mutating
  browser state.
- `browser_context.rs` is the deep browsing module: it holds a `browser::Tree`
  and adds everything the terminal puts around one — the viewport, the filter
  prompt with its directory descents and tab completion, the listing tasks,
  listing generation, task cancellation and stale-result rejection.

This split keeps high-level orchestration visible without fragmenting
`BrowserContext` into state, event and task files that would need to understand
the same invariants.

### GUI

Status: implemented. Outside `default`, because `eframe` brings a window system
and a GPU backend with it; `make lint` compiles it so it cannot rot.

```text
src/gui/
├── mod.rs
├── app.rs
├── dock.rs
├── glyph.rs
└── theme.rs
```

- `mod.rs` holds the binary's own concerns: argument parsing, the Tokio runtime
  the UI thread is entered *around* rather than wrapped in, the local connection,
  the window, and shutting the server down once the window is gone.
- `app.rs` owns `App` and the bridge between a synchronous immediate-mode UI and
  the asynchronous `Client`. Listings run as tasks that send groups over a
  bounded channel; a frame drains it with `try_recv`. Bounded, so the filesystem
  still feels backpressure when the UI is the slow one; `try_recv`, so the render
  thread never blocks. It also draws the chrome — title bar, breadcrumb, sidebar,
  status bar — and holds the expansion policy a click goes through.
- `dock.rs` owns a tab. `egui_dock` owns the layout, so there is no pane
  structure here: what a tab *contains* is the filter field, the listing as an
  `egui_extras` table, and a footer. A tab also holds its own filter query.
- `glyph.rs` paints the icons with the painter instead of loading assets, so the
  shapes sit on the pixel grid at the one size the list uses.
- `theme.rs` holds `Palette` and `Mode`. Every colour comes from `Palette`, so a
  theme swap is one struct, and egui's own defaults are overwritten wholesale.

The front end raises `Action`s rather than acting: a tab cannot reach the
connection or the runtime, so a click becomes `Toggle` or `Enter` for `App` to
apply after the frame. That is what keeps drawing free of task ownership, the
same rule `app/view.rs` follows in the TUI.

Both front ends drive `browser::Tree`; see [Browser model](#browser-model) for
the seam and why the viewport is where it falls.

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
- One module per subcommand, each driving a single `Client` workflow and
  printing what it observed. These modules add no behavior of their
  own; see [Client CLI](client-cli.md).
- `install.rs` and its bundled shell script own remote provisioning from an
  uploaded source tree. The script is `include_str!`'d, so it has to sit beside
  the module rather than in `scripts/`.

### Binary targets and features

`default` enables three of the four binaries, so `cargo test` and
`cargo clippy --all-targets` cover them without extra flags. `gui` is the
exception, and `make lint` and `make test` name it explicitly.

| Feature | Adds | Gated dependencies |
| --- | --- | --- |
| `client` | `client/` and the crate-root facade | async-trait, dirs, hyper-util, serde, tower, tracing, uuid |
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
2. Put one request against the selected host in `client/rpc`.
3. Put protobuf conversion and wire batching in `client/rpc/` and server
   `ops`. There is no second adapter to write: both targets use this one.
4. Put public multi-step ordering and lifecycle coordination in `Client`.
5. Touch `client/connection/` only for server lifecycle or transport, and put
   anything both targets share in `connection/mod.rs` rather than in
   `embedded.rs` and `ssh.rs` twice.
6. Add a UI adapter only when that boundary has a real consumer.

A public Client workflow does not require an identically named core, backend
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
  error unless a feature explicitly requires different semantics. Reserve a terminal
  error for a condition that really does end the stream: anything wrong with one item
  travels with that item, because ending the stream hides every item behind it.
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
- UI modules may consume `walk_dir`, but they should not recreate traversal,
  caching or transport policy.

### Checklist for adding a feature

1. Define the caller-visible semantics, especially streaming, partial-result,
   cancellation and error behavior.
2. Place the behavior at its semantic owner rather than copying the operation
   across all layers.
3. Add `fs` types and facade re-exports only when the behavior is
   transport-neutral.
4. Add an `rpc` module only for a new request against the selected host.
5. Add the protocol, server, and backend-adapter work that primitive needs. It
   is one path, so a capability that works for one target works for both.
6. Add Client composition for caller workflows.
7. Add UI adapters only for real consumers.
8. Keep tests beside the layer-specific invariant they verify.
9. Update this document when the new feature changes a responsibility boundary.

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

src/client/
├── directory/
│   ├── mod.rs
│   ├── list.rs
│   └── walk.rs
└── rpc/
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
