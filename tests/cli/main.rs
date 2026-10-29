//! Integration tests for the `argosy` binary: argument parsing, exit codes,
//! the stdout/stderr split, and the `--json` schema are the contract under test.
//! Everything here runs offline; tests needing the model weights are `#[ignore]`d.

mod agent;
mod catalog;
mod common;
mod convert;
mod help;
mod index;
mod init_validate;
mod package;
mod pull;
