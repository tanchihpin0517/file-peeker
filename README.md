# File Peeker

File Peeker is a native macOS application shell and Rust client/server
foundation for a future Finder-like file browser.

The client API and test CLI can install, start, and own a local server or
connect to one over SSH. Filesystem browsing is not implemented yet, and the
SwiftUI app does not start a session by default. Every window currently opens
to Home.

## Build

XcodeGen and Xcode are required.

```text
make xcode-build
scripts/run-app.sh
```

Generated Xcode projects and DerivedData are not committed.

## License

Licensed under the Apache License, Version 2.0. Contributors retain copyright
in their contributions while licensing them under Apache-2.0. See [LICENSE](LICENSE)
and [NOTICE](NOTICE).

Copyright 2026 Chih-Pin Tan.
