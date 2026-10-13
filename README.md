# File Peeker

File Peeker is a file browser for local and remote hosts. This repository is in
the middle of a rewrite.

- **v2**, at the repository root, is the rewrite: a persistent server on every
  machine that owns the tasks and the browsing session, with front ends as
  clients that attach, render and detach. Its design is
  [v2 architecture](docs/design/v2-architecture.md); every decision defaults to
  how [herdr](https://herdr.dev) does it. It is being built one step at a time and
  does not browse yet.
- **The shipping crate**, in [`old/`](old/README.md), is File Peeker as it works
  today: a terminal and desktop browser over local and SSH connections. It takes
  bug fixes only, and is deleted when v2 does everything it does.

## v2

A stable Rust toolchain is required; see [rust-toolchain.toml](rust-toolchain.toml).

```text
cargo run                            # the terminal UI, attached to this machine's server
cargo run -- status                  # versions, pid, protocol compatibility, uptime
cargo run -- task demo --seconds 8   # a task to watch in the UI's sidebar
cargo run -- server stop             # stop the server

cargo run -- machine add workbox me@workbox   # save an SSH machine, installing fpk there
cargo run -- --machine workbox status         # run a command on it
```

The server keeps running after `fpk` exits. Its sockets, lock and logs live in
`~/.file-peeker/` for a release build and `~/.file-peeker-dev/` for a dev build
such as `cargo run`, so the two never share a server; set `FILE_PEEKER_HOME` to
an absolute path to use another directory. The binary is `fpk` until cutover, because the shipping crate's
`file-peeker` holds that name.

## Checks

```text
make check       # v2: formatting, Clippy, tests
make old-check   # the shipping crate's own checks
make verify      # both, plus the documentation and shell script checks
```

The shell script check needs [ShellCheck](https://www.shellcheck.net).

## Documentation

- [TODO](TODO.md), the tracked work list for both crates
- [v2 documentation](docs/README.md), what v2 is built from:
  [task architecture](docs/task-architecture.md),
  [task pipeline](docs/task-pipeline.md) and [machines](docs/machines.md)
- [v2 architecture](docs/design/v2-architecture.md), the rewrite and its build order
- [Design proposals](docs/design/README.md), including the layout and palette galleries
- [The shipping crate's documentation](old/docs/README.md)

## License

Licensed under the Apache License, Version 2.0. Contributors retain copyright
in their contributions while licensing them under Apache-2.0. See [LICENSE](LICENSE)
and [NOTICE](NOTICE).

Copyright 2026 Chih-Pin Tan.
