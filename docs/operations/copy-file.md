# Copy File

`Session::op_copy_file_from` copies one regular file between any two Sessions.
It is destination-oriented: the receiving Session joins and validates the
destination directory and filename on its own selected host.

```rust
destination
    .op_copy_file_from(
        &source,
        source_path,
        destination_directory,
        destination_name,
        CopyConflict::FailIfExists,
    )
    .await
```

The same workflow covers every route:

| Source | Destination | Behavior |
| --- | --- | --- |
| Local | Local | Client-host streamed copy |
| Local | Remote | Upload |
| Remote | Local | Download |
| Remote | Remote | Relay through the client |

The source Session resolves and opens the source at copy time, then releases
its backend lock before consuming bytes. The destination backend similarly
returns an owned write future, so neither Session close is blocked by a long
transfer. Data remains streaming and bounded; remote frames contain at most
64 KiB.

The destination core writes a unique hidden partial file beside the final
path, flushes and closes it, then publishes it atomically. `FailIfExists` uses
an atomic hard-link create-if-absent operation and returns
`io::ErrorKind::AlreadyExists` without changing the existing entry. `Replace`
renames the completed partial over the destination. Source errors,
cancellation, disconnects, and task aborts remove the partial and never
publish truncated data.

Remote writes require one header, zero or more non-empty data frames, and one
explicit commit followed immediately by end-of-stream. A source failure ends
the RPC without commit, allowing the server to distinguish failure from a
valid empty file.

The result contains the final selected-host path and exact byte count. Version
1 copies regular-file contents only. It does not recurse into directories,
copy symlinks or special files, preserve metadata, resume, checksum, retain
sparse layout, or rename interactively.
