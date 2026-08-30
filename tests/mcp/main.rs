//! MCP integration tests: schemas, end-to-end over an in-process duplex,
//! and the code-intelligence tools.

#[cfg(feature = "code-tools")]
mod code_tools;
mod common;
mod e2e;
mod schemas;
