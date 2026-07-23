# Code Structure

This document records the intended code structure for File Peeker. It has three
parts:

1. the module structure currently adopted by the project;
2. placement and growth rules for future changes; and
3. implemented examples showing how those rules apply.

The goal is not to make every crate mirror every operation. Each layer should
own one coherent responsibility and expose the smallest useful interface to the
layer above it.

Detailed operation contracts do not live here. See the relevant document in
[Operations](README.md#add-or-change-a-feature) for caller-visible behavior,
failure policy and resource lifetime.

## Part I: Current module structure

### Responsibility boundaries

| Layer | Owns | Does not own |
| --- | --- | --- |
| `file-peeker-core` | Host-local filesystem behavior, transport-neutral types, streaming and cancellation semantics | Session selection, SSH, gRPC, UniFFI, UI behavior |
| `SessionBackend` | Primitive capabilities against the Session's selected host | Multi-step user workflows or operating-system UI actions |
| `Session` | Public, target-independent workflow entry points, backend lock/lifecycle, and selected-host/client-host phase ordering | Staging mechanics, system-command details, or protobuf |
| Client `FileService` | Client-host file preparation and operating-system workflows after selected-host data is acquired | Backend access, selected-host primitives, transport, or Session lifecycle |
| Remote backend modules | One gRPC request/response adapter per backend capability | Public workflow policy |
| Server `ops` | RPC validation, protobuf conversion, transport batching and delegation to core | Client workflows and UI state |
| `session/ffi` | UniFFI-compatible wrappers and error/state adaptation | Native Session behavior |
| TUI | Application state, input routing, rendering and task ownership | Filesystem and transport semantics |

The main boundary is:

```text
UI / FFI
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

### Core

Status: implemented.

```text
crates/file-peeker-core/src/
├── lib.rs
├── error.rs
├── fs_service.rs
├── resolve_path.rs
├── read.rs
└── directory/
    ├── mod.rs
    ├── entry.rs
    ├── list.rs
    └── walk.rs
```

- `lib.rs` is a thin crate facade and re-exports public types.
- `FsService` is the cohesive entry point for filesystem behavior and shared
  cancellation.
- `resolve_path.rs` and `read.rs` hold focused operations that do not yet form
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
crates/file-peeker-client/src/session/
├── mod.rs
├── path.rs
├── directory/
│   ├── mod.rs
│   ├── list.rs
│   └── walk.rs
├── file/
│   ├── mod.rs
│   ├── open.rs
│   ├── opener.rs
│   ├── service.rs
│   └── stage.rs
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
│       └── walk_dir.rs
└── ffi/
    ├── mod.rs
    └── listing.rs
```

- `session/mod.rs` owns Session construction, lifecycle and the private backend
  handle.
- `path.rs` contains Rust-native Session path workflows.
- `directory/` contains transport-neutral directory result types and native
  Session directory workflows.
- `file/open.rs` contains the Rust-native Session entry point and selected-host
  coordination.
- `file/service.rs` defines the private client `FileService` facade for
  client-host file preparation and operating-system workflows.
- `file/stage.rs` owns platform cache-root selection, lazy private-directory
  creation, safe streaming publication, and incomplete-file cleanup, while
  `file/opener.rs` owns the substitutable operating-system/test seam. They
  remain separate because cache policy and process launching are distinct
  responsibilities.
- `backend/mod.rs` defines the private `SessionBackend` capability seam.
- `backend/local.rs` adapts `FsService`; `backend/remote/` contains exact gRPC
  adapters for the same primitive capabilities.
- `backend/connection/` owns remote connection and server-process lifecycle,
  not filesystem operations.
- `ffi/` adapts native APIs for UniFFI. Its sticky listing state is a
  foreign-language boundary concern and should not leak back into the native
  stream.

`read_file` remains a private backend primitive rather than a caller-facing
native read operation. The Session open-file workflow consumes it directly:
local Sessions use it to validate and open the regular file before dropping the
stream, while remote Sessions consume the owned stream into a client-host cache.

### Server

Status: implemented.

```text
crates/file-peeker-server/src/
├── lib.rs
├── main.rs
├── server.rs
└── ops/
    ├── mod.rs
    ├── entry.rs
    ├── status.rs
    ├── list.rs
    ├── read.rs
    └── walk.rs
```

- `main.rs` is the thin CLI entry point.
- `server.rs` owns listener setup, authentication, health reporting, startup,
  shutdown and the `serve` lifecycle.
- `ops/mod.rs` is the tonic service adapter and keeps trivial delegation, such
  as path resolution, inline.
- `ops/list.rs`, `ops/read.rs`, and `ops/walk.rs` own behavior-heavy streaming
  conversion and batching; `ops/entry.rs` shares listing-entry conversion.
- `ops/status.rs` owns the status-to-gRPC error mapping shared by server
  operations.

There is no server backend trait. The server has one real filesystem
implementation, `FsService`; adding an interface there now would create a seam
without variation.

### TUI

Status: implemented.

```text
crates/file-peeker-tui/src/
├── main.rs
├── browser_context.rs
└── app/
    ├── mod.rs
    └── view.rs
```

- `main.rs` retains CLI parsing, terminal setup/restore, the event loop and
  direct key-to-command mapping.
- `app/mod.rs` owns `App`, application lifecycle, context routing and commands
  acting on the active context.
- `app/view.rs` renders application state without owning async tasks or mutating
  browser state.
- `browser_context.rs` is the deep browsing module: selected listing source,
  current path, entries, selection, listing generation, task cancellation,
  stale-result rejection and terminal listing state.

This split keeps high-level orchestration visible without fragmenting
`BrowserContext` into state, event and task files that would need to understand
the same invariants.

## Part II: Placement and growth rules

### Placement rule for a new operation

Before adding a file, identify the operation's semantic owner:

1. Put host-local filesystem mechanics in core.
2. Put a primitive selected-host capability in `SessionBackend`.
3. Implement local and remote adapters only when that capability must work on
   both targets.
4. Put public multi-step ordering and lifecycle coordination in `Session`.
5. Put post-backend client-host file preparation and OS actions in
   `FileService`.
6. Put protobuf conversion and wire batching in the remote adapter and server
   `ops`.
7. Add an FFI or UI adapter only when that boundary has a real consumer.

A public Session workflow does not require an identically named core, backend,
RPC and FFI operation. Layers should mirror semantics only where the semantics
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
- Avoid adding FFI and UI surface area until a concrete consumer needs it.

### Server growth

- Keep small validation/delegation methods in `ops/mod.rs`.
- Give each behavior-heavy streaming RPC its own `ops/<operation>.rs` module.
- Keep transport limits and protobuf conversion on the server side; core
  streams should not inherit gRPC message-size policy.
- Introduce shared operation infrastructure only after at least two operations
  need the same invariant.
- Do not add a server repository/backend abstraction while `FsService` remains
  the sole implementation.

### TUI growth

- Add `app/input.rs` and an `Action` type only when input gains modes,
  configurable bindings or a command palette.
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
3. Add core types and facade re-exports only when the behavior is
   transport-neutral.
4. Extend `SessionBackend` only for a selected-host primitive.
5. Add local and remote implementations, plus protocol and server work, only
   where remote execution requires them.
6. Add Session composition for caller workflows.
7. Place client-host file actions behind `FileService` only after Session has
   released its backend lock; selected-host primitives still follow the
   core/backend placement rules.
8. Add FFI and UI adapters only for real consumers.
9. Keep tests beside the layer-specific invariant they verify.
10. Update this document when the new feature changes a responsibility boundary.

## Part III: Implemented placement examples

### Recursive directory traversal

Recursive traversal is a separate `walk_dir` capability rather than a mode of
`list_dir`. Listing one directory and traversing a tree have different cost,
ordering, cancellation and error semantics.

Implemented structure:

```text
crates/file-peeker-core/src/directory/
├── mod.rs
├── entry.rs
├── list.rs
└── walk.rs

crates/file-peeker-client/src/session/
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

crates/file-peeker-server/src/ops/
├── mod.rs
├── entry.rs
├── list.rs
└── walk.rs
```

Core list and walk share entry inspection because classification is a
host-local filesystem concern. The client exposes separate directory workflows
and backend capabilities. The server shares entry conversion and transport
limits while retaining distinct List and Walk protobuf adapters. This keeps
different traversal semantics visible without duplicating shared mechanics.

Caller-visible behavior is documented in [Client API](api.md); remote batching
and validation are owned by [Remote Protocol](protocol.md).

### Opening a selected-host file

Opening a selected-host file demonstrates a public Session workflow composed
from backend primitives and a client-host service:

```text
Session::op_open_file(path)
    -> SessionBackend::resolve_path
    -> SessionBackend::read_file
    -> release the backend read lock
    -> FileService
         |-- local: drop the validation stream and use the resolved path
         `-- remote: FileStager::stage_download
         -> FileOpener
```

The backend seam supplies selected-host primitives. Session owns backend
lifecycle and phase ordering. `FileService` owns client-host preparation and
operating-system integration but cannot access a backend. It is a workflow
facade, not a replacement for core `FsService` or `SessionBackend`.

The file domain is:

```text
crates/file-peeker-client/src/session/
├── file/
│   ├── mod.rs
│   ├── open.rs
│   ├── opener.rs
│   ├── service.rs
│   └── stage.rs
```

The detailed validation, staging, cache, cancellation and retention contract is
owned by [Open File](operations/open-file.md).
