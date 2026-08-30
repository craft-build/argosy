//! Unit tests for packaging, integrity, and YAML import.

mod import;
mod packaging;

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::bundle::{Argosy, Finding, Severity};
use crate::concept::Concept;
use crate::error::Error;
use crate::hash::sha256_hex;
use crate::local::LocalArgosy;

/// `validate_styleguide` returns raw findings; these are the error-severity ones.
fn error_findings(argosy: &Argosy) -> Vec<Finding> {
    argosy
        .validate_styleguide()
        .into_iter()
        .filter(|f| f.severity == Severity::Error)
        .collect()
}

const MANIFEST: &str = "---\ntype: Argosy Manifest\nname: acme-billing\nargosy_version: 0.3.1\n---\n\nThe acme billing knowledge bundle.\n";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The full fixture from the doc-08 success criteria: every reserved
/// namespace, a custom one, a `.argosy/` index, and a nested
/// `document/memory-notes/` that must NOT be caught by the exclusion.
fn fixture_argosy(dir: &TempDir) -> PathBuf {
    let root = dir.path().join("fixture");
    write(&root, "argosy.md", MANIFEST);
    write(
        &root,
        "document/design.md",
        "---\ntype: Document\ndescription: Design notes.\n---\n\n# Design\n",
    );
    write(
        &root,
        "document/memory-notes/notes.md",
        "---\ntype: Document\ndescription: About memory, not memory itself.\n---\n\nnotes\n",
    );
    write(
        &root,
        "memory/gotchas.md",
        "---\ntype: Memory\ndescription: Private scratch.\n---\n\ngotcha\n",
    );
    write(
        &root,
        "custom/product/faq.md",
        "---\ntype: Note\ndescription: Producer-owned custom namespace.\n---\n\nfaq\n",
    );
    write(&root, ".argosy/index.db", "sqlite bytes");
    root
}

fn import_fixture(dir: &TempDir) -> LocalArgosy {
    let root = dir.path().join("local");
    write(&root, "argosy.md", MANIFEST);
    LocalArgosy::open(&root).unwrap()
}

const RUST_RULES: &str = "\
- id: no-unwrap-in-prod
  description: Do not call unwrap outside tests.
  language: rust
  category: error-handling
  priority: error
  pattern: \".unwrap()\"
  good:
    - \"let value = maybe()?;\"
    - \"let value = maybe.expect('known here');\"
  bad: \"let value = maybe.unwrap();\"
- id: minimal-rule
  description: A rule with only the required fields.
";

const MAPPING_RULES: &str = "\
rules:
  - id: no-eval
    description: Never evaluate strings as code.
    language: python
    priority: warn
    good: \"ast.literal_eval(text)\"
    bad:
      - \"eval(text)\"
";

// The Craft schema shape (styleguide-schema.json): facets live in the
// file-level `metadata:` block, examples nest under `examples:`, and
// `priority` may be `warning`/`hint`.
const CRAFT_SCHEMA_RULES: &str = "\
metadata:
  name: \"Rust Naming Conventions\"
  version: \"1.0.0\"
  language: rust
  category: naming
rules:
  - id: SNAKE-CASE-VARS
    description: Use snake_case for variables and functions.
    priority: warning
    pattern: \"^[a-z][a-z0-9_]*$\"
    examples:
      good:
        - \"let my_variable = 5;\"
      bad:
        - \"let MyVariable = 5;\"
    tags: [naming, convention]
  - id: OVERRIDE-EXPLICIT-FACETS
    description: Per-rule facets beat the file-level metadata defaults.
    language: rust-2021
    category: style
  - id: HINT-PRIORITY-KEPT
    description: The hint priority passes through verbatim.
    priority: hint
";
