# Open File

This document owns the caller-visible behavior, staging policy and cache
lifetime for opening a selected-host file with an application on the client
host.

## Contract

`Session::op_open_file(path)` resolves and validates a regular file on the
selected host, prepares a client-host path, and asks the client operating system
to open it with the default application. Its UniFFI equivalent is
`opOpenFileUniffi`.

The production system opener currently supports macOS only. On other platforms,
validation or remote staging may complete, but the final open request returns an
unsupported-operation error.

## Responsibility flow

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

The backend supplies selected-host filesystem primitives only. Session owns the
ordering and backend lifetime. `FileService` starts after Session releases the
backend read lock and owns client-host preparation and operating-system
integration; it cannot access a backend.

Neither core nor the remote server launches an application on the client host.
The existing streaming Read capability supplies remote bytes, so the protocol
does not need a materialization or open RPC.

## Local behavior

The local backend opens and validates the resolved path as a regular file.
`FileService` then drops the unused validation stream and passes the existing
resolved path to `FileOpener`. It does not create a cache directory or copy the
file.

## Remote staging

Remote files are completely staged before `FileOpener` is invoked. Each open
uses a fresh unique directory because the protocol currently exposes no
freshness metadata:

```text
<cache root>/
└── <Session UUID>/
    └── <per-open UUID>/
        └── <remote basename>
```

Only the remote basename is retained. Staging writes to a private sibling
partial file, flushes it, closes it, and atomically renames it to the final
basename. A failed or cancelled download removes the incomplete partial file and
does not invoke the opener. A completed staged file remains present even if the
subsequent opener request fails.

Session close does not wait for a remote copy: the owned read stream no longer
holds the backend lock, so closing the remote backend cancels the stream and the
open operation reports that terminal error.

## Cache root and permissions

The production staging root is resolved from the platform cache base:

- macOS: `~/Library/Caches/FilePeeker/open-files`
- Linux with `XDG_CACHE_HOME`: `$XDG_CACHE_HOME/file-peeker/open-files`
- Linux otherwise: `$HOME/.cache/file-peeker/open-files`
- Other platforms: their native cache base followed by
  `file-peeker/open-files`
- No valid absolute cache base: `<system temp>/file-peeker/open-files`

The hierarchy is created lazily only when a remote file is staged. On Unix, the
File Peeker-owned cache root, Session directory and per-open directory use mode
`0700`; staged files use mode `0600`. The surrounding platform cache base is not
modified.

## Retention

Only incomplete partial files are removed automatically. Completed files survive
the open operation, `FileService` destruction and Session shutdown because
operating-system acceptance does not mean the receiving application has
finished reading the file.

TTL, LRU, size limits, startup cleanup and a manual clear-cache operation are
future policy possibilities and are not implemented.

