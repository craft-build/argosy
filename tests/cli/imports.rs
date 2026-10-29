//! `update` / `remove` / `sync` and the pull lockfile: the pin contract.

use std::fs;
use std::path::Path;
use std::process::Command;

use predicates::prelude::*;

use tempfile::TempDir;

use argosy::lockfile::Lockfile;
use argosy::pull;

use super::common::*;

/// Runs git in `dir`, panicking with git's stderr on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit(dir: &Path, message: &str) {
    git(
        dir,
        &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"],
    );
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

fn head_sha(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project cwd plus its isolated XDG state home and store, and a book
/// repo whose first commit already carries a skill file.
struct Setup {
    scratch: TempDir,
    project: std::path::PathBuf,
    xdg: std::path::PathBuf,
    repo: std::path::PathBuf,
    store: std::path::PathBuf,
}

impl Setup {
    fn new() -> Self {
        let scratch = TempDir::new().unwrap();
        let repo = fixture_copy("valid-acme-billing", &scratch);
        fs::create_dir_all(repo.join("skill")).unwrap();
        fs::write(repo.join("skill/one.md"), "old line\n").unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"]);
        commit(&repo, "one");

        let project = scratch.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let xdg = xdg_state_home(&scratch);
        let store = pull::project_argosy_dir_at(&xdg.join("argosy"), &project);
        Self {
            scratch,
            project,
            xdg,
            repo,
            store,
        }
    }

    fn url(&self) -> &str {
        self.repo.to_str().unwrap()
    }

    fn argosy(&self) -> assert_cmd::Command {
        let mut cmd = argosy_bin();
        cmd.current_dir(&self.project)
            .env("XDG_STATE_HOME", &self.xdg);
        cmd
    }
}

#[test]
fn pull_records_a_pin_in_the_lockfile() {
    let setup = Setup::new();

    setup
        .argosy()
        .args(["pull", "--json", setup.url(), "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"commit\""));

    let lock = Lockfile::load(&setup.store).unwrap();
    let entry = lock.entry("company-rules").unwrap();
    assert_eq!(entry.url, setup.url());
    assert_eq!(entry.commit, head_sha(&setup.repo));
    assert_eq!(entry.integrity, "no sidecar");
    assert!(!entry.reference.is_empty());
}

#[test]
fn pull_reproduces_the_pin_when_the_checkout_is_missing() {
    let setup = Setup::new();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();
    let pinned = head_sha(&setup.repo);

    // Upstream moves past the pin…
    fs::write(setup.repo.join("skill/one.md"), "new line\n").unwrap();
    commit(&setup.repo, "two");

    // …and the checkout disappears: re-pulling reproduces the pinned
    // commit, not the new HEAD.
    fs::remove_dir_all(setup.store.join("company-rules")).unwrap();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();

    assert_eq!(
        pull::checkout_head_commit(&setup.store.join("company-rules")).as_deref(),
        Some(pinned.as_str()),
        "the pin wins over the remote's HEAD"
    );
    let lock = Lockfile::load(&setup.store).unwrap();
    assert_eq!(lock.entry("company-rules").unwrap().commit, pinned);
}

#[test]
fn pull_refuses_a_pinned_name_from_a_different_url() {
    let setup = Setup::new();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();
    // A distinct second remote (a separate path, not the same fixture copy).
    let other = setup.scratch.path().join("other-remote");
    copy_dir(&fixture("valid-acme-billing"), &other);
    git_commit_all(&other);
    fs::remove_dir_all(setup.store.join("company-rules")).unwrap();

    setup
        .argosy()
        .args(["pull", other.to_str().unwrap(), "company-rules"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("is pinned to"));
}

#[test]
fn update_previews_then_applies_the_reviewed_change() {
    let setup = Setup::new();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();
    fs::write(setup.repo.join("skill/one.md"), "new line\n").unwrap();
    commit(&setup.repo, "two");
    let second = head_sha(&setup.repo);

    // Dry run: the diff is shown, nothing moves.
    setup
        .argosy()
        .args(["update", "--dry-run", "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-old line"))
        .stdout(predicate::str::contains("+new line"))
        .stdout(predicate::str::contains("dry run"));
    let lock = Lockfile::load(&setup.store).unwrap();
    assert_ne!(lock.entry("company-rules").unwrap().commit, second);

    // Applied: the pin moves and the lockfile says so.
    setup
        .argosy()
        .args(["update", "--yes", "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved the pin"));
    assert_eq!(
        pull::checkout_head_commit(&setup.store.join("company-rules")).as_deref(),
        Some(second.as_str())
    );
    assert_eq!(
        Lockfile::load(&setup.store)
            .unwrap()
            .entry("company-rules")
            .unwrap()
            .commit,
        second
    );

    // No change left.
    setup
        .argosy()
        .args(["update", "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already at"));
}

#[test]
fn update_refuses_to_move_a_pin_without_confirmation() {
    let setup = Setup::new();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();
    let first = head_sha(&setup.repo);
    fs::write(setup.repo.join("skill/one.md"), "new line\n").unwrap();
    commit(&setup.repo, "two");

    // assert_cmd pipes stdin, so this is the non-terminal path.
    setup
        .argosy()
        .args(["update", "company-rules"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "refusing to move the pin without confirmation",
        ));

    assert_eq!(
        pull::checkout_head_commit(&setup.store.join("company-rules")).as_deref(),
        Some(first.as_str()),
        "the refusal moved nothing"
    );
}

#[test]
fn remove_deletes_the_checkout_its_stamp_and_its_pin() {
    let setup = Setup::new();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();

    setup
        .argosy()
        .args(["remove", "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed"))
        .stdout(predicate::str::contains("pin dropped"));

    assert!(!setup.store.join("company-rules").exists());
    assert!(
        !pull::import_integrity_stamp(&setup.store.join("company-rules")).exists(),
        "the stamp beside the checkout is gone too"
    );
    assert!(
        Lockfile::load(&setup.store)
            .unwrap()
            .entry("company-rules")
            .is_none()
    );

    setup
        .argosy()
        .args(["remove", "company-rules"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no checkout"));
}

#[test]
fn sync_materializes_carried_pins_and_reports_drift() {
    let machine_a = Setup::new();
    machine_a
        .argosy()
        .args(["pull", machine_a.url(), "company-rules"])
        .assert()
        .success();
    let carried = machine_a.store.join("argosy.lock");
    let first_pin = Lockfile::load(&machine_a.store)
        .unwrap()
        .entry("company-rules")
        .unwrap()
        .clone();

    // A second machine with an empty state adopts the carried pin…
    let machine_b = Setup::new();
    machine_b
        .argosy()
        .args(["sync", "--from", carried.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("pulled"))
        .stdout(predicate::str::contains("company-rules"));
    assert_eq!(
        pull::checkout_head_commit(&machine_b.store.join("company-rules")).as_deref(),
        Some(first_pin.commit.as_str()),
        "sync clones at the pin, not at HEAD"
    );

    // …and is at pin on a second run.
    machine_b
        .argosy()
        .args(["sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("at pin"));

    // Machine A moves its pin; adopting it on B leaves B's checkout at the
    // old commit: drift, reported with a hint.
    fs::write(machine_a.repo.join("skill/one.md"), "new line\n").unwrap();
    commit(&machine_a.repo, "two");
    machine_a
        .argosy()
        .args(["update", "--yes", "company-rules"])
        .assert()
        .success();
    machine_b
        .argosy()
        .args(["sync", "--from", carried.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("drifted"))
        .stdout(predicate::str::contains("argosy update company-rules"));

    // Moving the drifted checkout to the adopted pin is an ordinary update.
    machine_b
        .argosy()
        .args(["update", "--yes", "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved the pin"));
    machine_b
        .argosy()
        .args(["sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("at pin"));
}

#[test]
fn update_reconciles_a_stale_pin() {
    let setup = Setup::new();
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();
    let head = head_sha(&setup.repo);

    // A pin that disagrees with the checkout (a crash between apply and
    // the lock write leaves exactly this state).
    let mut lock = Lockfile::load(&setup.store).unwrap();
    let mut entry = lock.entry("company-rules").unwrap().clone();
    entry.commit = "0123456789abcdef0123456789abcdef01234567".to_string();
    lock.upsert("company-rules", entry);
    lock.save(&setup.store).unwrap();

    setup
        .argosy()
        .args(["update", "company-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already at"))
        .stdout(predicate::str::contains("reconciled the pin"));

    assert_eq!(
        Lockfile::load(&setup.store)
            .unwrap()
            .entry("company-rules")
            .unwrap()
            .commit,
        head
    );
    setup
        .argosy()
        .args(["sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("at pin"));
}

/// A packaged remote whose clones carry an integrity sidecar, committed as
/// a git repo, plus the standard project/store layout.
fn packaged_setup(scratch: TempDir) -> Setup {
    let source = scratch.path().join("packaged-source");
    fs::create_dir_all(source.join("skill")).unwrap();
    fs::write(
        source.join("argosy.md"),
        "---\ntype: Argosy Manifest\nname: sealed-rules\nargosy_version: \"1.0.0\"\n---\n# sealed\n",
    )
    .unwrap();
    fs::write(source.join("skill/one.md"), "old line\n").unwrap();
    let repo = scratch.path().join("sealed-repo");
    argosy::package::package(
        &argosy::Argosy::open(&source).unwrap(),
        &repo,
        &argosy::package::PackageOptions::default(),
    )
    .unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    commit(&repo, "one");

    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let xdg = xdg_state_home(&scratch);
    let store = pull::project_argosy_dir_at(&xdg.join("argosy"), &project);
    Setup {
        scratch,
        project,
        xdg,
        repo,
        store,
    }
}

#[test]
fn sync_reports_integrity_failure_for_a_tampered_worktree() {
    let setup = packaged_setup(TempDir::new().unwrap());
    setup
        .argosy()
        .args(["pull", setup.url(), "company-rules"])
        .assert()
        .success();

    // Tamper the checkout's worktree (HEAD unmoved): "at pin" alone would
    // present a book as healthy that the serving path excludes.
    fs::write(setup.store.join("company-rules/skill/one.md"), "injected\n").unwrap();

    setup
        .argosy()
        .args(["sync"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("integrity failed"))
        .stdout(predicate::str::contains(
            "sync: 0 pulled, 0 at pin, 0 drifted, 1 failed",
        ));
}

#[test]
fn sync_fails_loudly_when_a_pin_cannot_be_reproduced() {
    let setup = Setup::new();
    // A pin for a commit no remote ever had.
    let mut lock = Lockfile::empty();
    lock.upsert(
        "company-rules",
        argosy::lockfile::LockEntry {
            url: setup.url().to_string(),
            reference: "main".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            integrity: "no sidecar".to_string(),
        },
    );
    lock.save(&setup.store).unwrap();

    setup
        .argosy()
        .args(["sync"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("failed"))
        .stdout(predicate::str::contains("force-pushed"));

    assert!(
        !setup.store.join("company-rules").exists(),
        "a failed reproduction leaves no checkout"
    );
}
