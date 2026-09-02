//! Integration tests for the `argosy` binary. These drive the
//! compiled binary, not library calls: argument parsing, exit codes, the
//! stdout/stderr split, and the `--json` schema are the contract under test.
//! Everything here runs offline; the real-backend index tests that need the
//! downloaded model weights are `#[ignore]`d like the backend tests.

mod agent;
mod common;
mod convert;
mod help;
mod index;
mod init_validate;
mod package;
mod pull;
