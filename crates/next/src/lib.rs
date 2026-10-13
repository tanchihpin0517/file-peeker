//! File Peeker v2: a persistent server per machine, run by tasks.
//!
//! This crate is the rewrite proposed in `docs/design/v2-architecture.md`. It
//! starts empty and grows one build step at a time; it depends on nothing in the
//! shipping crate and replaces it at cutover.
