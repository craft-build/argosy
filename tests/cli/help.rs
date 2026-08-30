//! `--help` documentation tests.

use predicates::prelude::*;

use super::common::*;

#[test]
fn help_documents_the_package_memory_guarantee() {
    argosy_bin()
        .args(["package", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("memory"))
        .stdout(predicate::str::contains("NEVER included"));
}

#[cfg(feature = "mcp")]
#[test]
fn help_documents_the_mcp_stdio_transport_and_model_download() {
    // The model download fires on `mcp` first runs too, not just
    // `index build` — the help must say so.
    argosy_bin()
        .args(["mcp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stdio"))
        .stdout(predicate::str::contains("90 MB"));
}

#[cfg(feature = "default-index")]
#[test]
fn help_documents_the_index_default_location_and_model_download() {
    argosy_bin()
        .args(["index", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("index.db"))
        .stdout(predicate::str::contains("state dir"));

    argosy_bin()
        .args(["index", "build", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("90 MB"))
        .stdout(predicate::str::contains("download"));
}
