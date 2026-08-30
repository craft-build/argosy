//! Unit tests for the MCP surface: the rig opens one local fixture argosy
//! (`acme-billing`) plus one imported fixture argosy (`acme-shared`).

mod promote;
mod prompts;
mod resources;
mod search;
mod sessions;
mod skills;
mod writes;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

use super::*;
use crate::LocalArgosy;
use crate::bundle::Namespace;
use crate::concept::Concept;
use crate::context::ProjectContext;
use crate::error::Error;
use crate::index::Index;
use crate::index::tests::{MemStore, MockEmbedder};
use crate::testutil::fixture_copy;

/// An imported argosy named `acme-shared` with one verified skill.
fn import_fixture() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let local = LocalArgosy::init(tmp.path(), Some("acme-shared"), None).unwrap();
    let skill: Concept = ("---\n\
         type: Skill\n\
         description: Audit the shared provisioner.\n\
         verified: machine-confirmed\n\
         ---\n\
         # Audit\n\n\
         Steps.\n")
        .parse()
        .unwrap();
    local
        .write_concept(
            Namespace::Skill,
            &"skill/shared-audit".parse().unwrap(),
            &skill,
        )
        .unwrap();
    tmp
}

struct Rig {
    _local: TempDir,
    _imported: TempDir,
    state: McpState<MockEmbedder, MemStore>,
}

/// A session factory over fixed roots — the unit-test double of the CLI's
/// open-on-demand factory: every cwd maps to the same fixture session,
/// freshly opened and reconciled on first use (then cached).
fn factory(local: PathBuf, imported: Vec<PathBuf>) -> SessionFactory<MockEmbedder, MemStore> {
    Arc::new(move |_root| {
        let context = ProjectContext::open(&local, imported.clone())?;
        let mut index = Index::new(MockEmbedder::new(), MemStore::new());
        index.reconcile(&context)?;
        Ok(ProjectSession::new(context, index))
    })
}

fn rig() -> Rig {
    let local = fixture_copy("valid-acme-billing");
    let imported = import_fixture();
    let state = McpState::new(factory(
        local.path().to_path_buf(),
        vec![imported.path().to_path_buf()],
    ));
    Rig {
        _local: local,
        _imported: imported,
        state,
    }
}

/// The cwd every rig test passes: the factory ignores it (any root maps
/// to the fixture session), so a constant stands in for a real project.
fn project() -> PathBuf {
    PathBuf::from("/project")
}
