//! The `conflicts` tool: find and resolve git merge-conflict markers,
//! ported from Craft's `tools/conflicts.rs`.
//!
//! Port note: unlike Craft, which wrote resolutions with a bare
//! `std::fs::write`, the resolve path here goes through the same
//! read → stale-read check → write discipline as `astgrep` apply: every file
//! read is recorded, and a write is refused when the file changed since.

#[cfg(feature = "mcp")]
use rmcp::schemars;

use serde::{Deserialize, Serialize};

use super::{CodeTools, relative_path, resolve_path, tool_error};
use crate::error::Result;

const CONFLICT_START: &str = "<<<<<<< ";
const CONFLICT_SEPARATOR: &str = "=======";
const CONFLICT_BASE: &str = "|||||||";
const CONFLICT_END: &str = ">>>>>>> ";
const THEIRS: &str = "@theirs";
const OURS: &str = "@ours";
const BASE: &str = "@base";

/// `conflicts` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct ConflictsParams {
    /// Directory (or file) to scan (default: the server's working
    /// directory).
    pub path: Option<String>,
    /// Resolve conflicts instead of listing. `"@theirs"` keeps the incoming
    /// (their branch) side, `"@ours"` keeps the current (our branch) side,
    /// `"@base"` drops both sides. Omit to list only.
    pub resolve: Option<String>,
    /// Resolve only the Nth conflict (1-indexed) per file. Omit to resolve
    /// all conflicts in scope.
    pub index: Option<usize>,
}

/// The `conflicts` outcome.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictsReport {
    /// The resolved scope path.
    pub path: String,
    /// The rendered listing (or resolution summary).
    pub text: String,
    /// Number of conflicts resolved; present only in resolve mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<usize>,
}

/// Runs the `conflicts` tool: list markers, or resolve them per `resolve`.
pub fn run(code: &CodeTools, params: ConflictsParams) -> Result<ConflictsReport> {
    let scope = params.path.as_deref().unwrap_or(".").to_string();
    let scope_path = resolve_path(&scope)?;

    if let Some(choice) = params.resolve.as_deref() {
        if params.index == Some(0) {
            return Err(tool_error(
                "index is 1-indexed; pass 1 for the first conflict, or omit it to resolve all",
            ));
        }
        let side = match choice {
            THEIRS => ConflictSide::Theirs,
            OURS => ConflictSide::Ours,
            BASE => ConflictSide::Base,
            other => {
                return Err(tool_error(format!(
                    "unknown resolve choice \"{other}\"; use {THEIRS}, {OURS}, or {BASE}"
                )));
            }
        };
        // Mutating runs serialize against each other (see
        // `CodeTools::begin_write`) so the per-file stale-read check cannot
        // race a concurrent resolve's write.
        let _write_guard = code.begin_write();
        let scope = resolve_in_scope(code, &scope_path, side, params.index);

        let mut text = if scope.resolved_files.is_empty() {
            format!(
                "no conflicts resolved ({} found, {} remaining)",
                scope.total_resolved, scope.remaining
            )
        } else {
            let mut out = format!(
                "resolved {} conflict(s) as {choice} in {} file(s):\n",
                scope.total_resolved,
                scope.resolved_files.len()
            );
            for f in &scope.resolved_files {
                out.push_str(&format!("  {f}\n"));
            }
            if scope.remaining > 0 {
                out.push_str(&format!(
                    "{} conflict(s) remain unresolved\n",
                    scope.remaining
                ));
            }
            out
        };
        for warning in &scope.skipped {
            text.push_str(&format!("warning: {warning}\n"));
        }
        return Ok(ConflictsReport {
            path: relative_path(&scope_path),
            text,
            resolved: Some(scope.total_resolved),
        });
    }

    let (conflicts, warnings) = collect_conflicts(code, &scope_path);

    let text = if conflicts.is_empty() && warnings.is_empty() {
        "no merge conflicts found".to_string()
    } else {
        let mut out = format!("merge conflicts in {} file(s):\n", conflicts.len());
        for (file, markers) in &conflicts {
            out.push_str(&format!("\n{file} ({} conflict(s)):\n", markers.len()));
            for m in markers {
                out.push_str(&format!(
                    "  {} - {}: {} vs {}\n",
                    m.start_line, m.end_line, m.our_branch, m.their_branch
                ));
            }
        }
        for warning in &warnings {
            out.push_str(&format!("warning: {warning}\n"));
        }
        out
    };
    Ok(ConflictsReport {
        path: relative_path(&scope_path),
        text,
        resolved: None,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct ConflictMarker {
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) our_branch: String,
    pub(crate) their_branch: String,
}

fn parse_conflicts(content: &str) -> Vec<ConflictMarker> {
    let mut markers = Vec::new();
    let mut current: Option<ConflictMarker> = None;

    for (i, raw) in content.lines().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(branch) = line.strip_prefix(CONFLICT_START) {
            current = Some(ConflictMarker {
                start_line: i + 1,
                end_line: 0,
                our_branch: branch.trim().to_string(),
                their_branch: String::new(),
            });
        } else if line == CONFLICT_SEPARATOR && current.is_some() {
        } else if let Some(branch) = line.strip_prefix(CONFLICT_END)
            && let Some(mut m) = current.take()
        {
            m.end_line = i + 1;
            m.their_branch = branch.trim().to_string();
            markers.push(m);
        }
    }

    markers
}

#[derive(Clone, Copy)]
enum ConflictSide {
    Ours,
    Theirs,
    Base,
}

/// One file's resolution outcome.
struct ResolvedFile {
    /// The rewritten content; meaningful only when `unterminated` is `None`.
    content: String,
    /// Number of complete conflicts resolved.
    resolved: usize,
    /// 1-based line of a conflict opener that never reached its `>>>>>>> `
    /// end marker. When set, the rewrite is abandoned: everything from the
    /// stray opener to EOF is ambiguous, so the file must be left untouched
    /// rather than silently truncated.
    unterminated_at: Option<usize>,
}

/// Rewrite a file's content, resolving conflict markers according to `side`.
/// `index` selects only the Nth (1-indexed) conflict; `None` resolves all.
/// Conflicts outside the selection are re-emitted verbatim, original branch
/// labels included.
fn resolve_content(content: &str, side: ConflictSide, index: Option<usize>) -> ResolvedFile {
    let mut out = String::with_capacity(content.len());
    // 0 = outside a conflict, 1 = ours, 2 = theirs, 3 = diff3 base.
    let mut state = 0u8;
    let mut ours = String::new();
    let mut theirs = String::new();
    // diff3 and zdiff3 conflict styles (`merge.conflictStyle = diff3`)
    // interpose a `|||||||` base section between ours and the separator.
    // Every side choice drops it (`@base` drops the whole conflict), but
    // a preserved block re-emits it verbatim, original label included.
    let mut base = String::new();
    let mut base_marker_line = String::new();
    let mut opener_line = String::new();
    let mut separator_line = String::new();
    let mut opener_at = 0usize;
    let mut count = 0usize;
    let mut resolved = 0usize;

    for (i, line) in content.split_inclusive('\n').enumerate() {
        // Tolerate CRLF checkouts: the markers compare on the bare line, the
        // original bytes are re-emitted verbatim when a block is preserved.
        // Each strip falls back to its own input — falling back to `line`
        // would re-attach the stripped newline.
        let bare = line.strip_suffix('\n').unwrap_or(line);
        let bare = bare.strip_suffix('\r').unwrap_or(bare);
        match state {
            0 => {
                if bare.starts_with(CONFLICT_START) {
                    count += 1;
                    state = 1;
                    ours.clear();
                    theirs.clear();
                    base.clear();
                    base_marker_line.clear();
                    opener_line = line.to_string();
                    opener_at = i + 1;
                } else {
                    out.push_str(line);
                }
            }
            1 => {
                if bare == CONFLICT_SEPARATOR {
                    separator_line = line.to_string();
                    state = 2;
                } else if bare.starts_with(CONFLICT_BASE) {
                    base_marker_line = line.to_string();
                    state = 3;
                } else {
                    ours.push_str(line);
                }
            }
            3 => {
                if bare == CONFLICT_SEPARATOR {
                    separator_line = line.to_string();
                    state = 2;
                } else {
                    base.push_str(line);
                }
            }
            _ => {
                if bare.starts_with(CONFLICT_END) {
                    let keep = index.is_some_and(|n| n != count);
                    if keep {
                        out.push_str(&opener_line);
                        out.push_str(&ours);
                        if !base_marker_line.is_empty() {
                            out.push_str(&base_marker_line);
                            out.push_str(&base);
                        }
                        out.push_str(&separator_line);
                        out.push_str(&theirs);
                        out.push_str(line);
                    } else {
                        let target = match side {
                            ConflictSide::Ours => ours.as_str(),
                            ConflictSide::Theirs => theirs.as_str(),
                            ConflictSide::Base => "",
                        };
                        out.push_str(target);
                        resolved += 1;
                    }
                    state = 0;
                } else {
                    theirs.push_str(line);
                }
            }
        }
    }
    if state != 0 {
        return ResolvedFile {
            content: String::new(),
            resolved: 0,
            unterminated_at: Some(opener_at),
        };
    }
    ResolvedFile {
        content: out,
        resolved,
        unterminated_at: None,
    }
}

/// Files resolved, counts for the summary, and warnings for files skipped
/// because they end inside an unterminated conflict block.
struct ScopeResolution {
    resolved_files: Vec<String>,
    total_resolved: usize,
    remaining: usize,
    skipped: Vec<String>,
}

fn resolve_in_scope(
    code: &CodeTools,
    scope_path: &str,
    side: ConflictSide,
    index: Option<usize>,
) -> ScopeResolution {
    let (files, skipped, remaining) = collect_conflict_files(scope_path);
    let mut out = ScopeResolution {
        resolved_files: Vec::new(),
        total_resolved: 0,
        remaining,
        skipped,
    };

    for (path, content) in files {
        let file = resolve_content(&content, side, index);
        let marker_count = parse_conflicts(&content).len();
        if let Some(line) = file.unterminated_at {
            tracing::warn!(
                path = %path,
                opener_line = line,
                "conflicts resolve: unterminated conflict block, leaving file untouched"
            );
            out.skipped.push(format!(
                "{}: conflict opener at line {line} has no >>>>>>> end marker; file left untouched — fix the marker or resolve manually",
                relative_path(&path)
            ));
            out.remaining += marker_count;
            continue;
        }
        out.remaining += marker_count.saturating_sub(file.resolved);
        if file.resolved > 0 {
            // The stale-read guard compares against the last read from an
            // earlier tool call (zoom, ...); this scan deliberately does not
            // record its reads first, so the guard stays meaningful.
            if let Err(message) = code.check_before_edit(std::path::Path::new(&path)) {
                tracing::warn!(
                    path = %path,
                    %message,
                    "conflicts resolve: skipping file"
                );
                out.remaining += file.resolved;
                continue;
            }
            if let Err(e) = std::fs::write(&path, &file.content) {
                tracing::warn!(path = %path, error = %e, "conflicts resolve: write failed");
                out.remaining += file.resolved;
                continue;
            }
            code.record_read(std::path::Path::new(&path));
            out.total_resolved += file.resolved;
            out.resolved_files.push(relative_path(&path));
        }
    }
    out
}

/// Files with conflict markers in scope, split by readability: UTF-8 files
/// (resolvable here) and files whose raw bytes carry markers but which are
/// not valid UTF-8 — those stay conflicted and unresolvable by this tool,
/// so they must be surfaced, never silently dropped.
#[derive(Default)]
struct ScannedConflicts {
    files: Vec<(String, String)>,
    non_utf8: Vec<(String, usize)>,
}

fn scan_conflict_files(code: Option<&CodeTools>, scope_path: &str) -> ScannedConflicts {
    let builder = ignore::WalkBuilder::new(scope_path)
        .hidden(true)
        .git_ignore(true)
        .build();
    let mut out = ScannedConflicts::default();
    for entry in builder.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        match std::fs::read_to_string(path) {
            Ok(content) => {
                // Listing mode records its reads (feeding the stale-read
                // guard); resolve mode deliberately does not, so its guard
                // keeps comparing against the last read from an earlier
                // tool call.
                if let Some(code) = code {
                    code.record_read(path);
                }
                if content.contains(CONFLICT_START) {
                    out.files
                        .push((path.to_string_lossy().into_owned(), content));
                }
            }
            Err(_) => {
                // Not valid UTF-8 (or unreadable): check the raw bytes so a
                // conflicted file in another encoding or a mangled checkout
                // cannot hide behind the failed read.
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                let marker = CONFLICT_START.as_bytes();
                let count = bytes.windows(marker.len()).filter(|w| *w == marker).count();
                if count > 0 {
                    out.non_utf8
                        .push((path.to_string_lossy().into_owned(), count));
                }
            }
        }
    }
    out
}

/// Resolve-mode view of the scan: resolvable files plus warnings and a
/// remaining-count contribution for the non-UTF-8 ones.
fn collect_conflict_files(scope_path: &str) -> (Vec<(String, String)>, Vec<String>, usize) {
    let scanned = scan_conflict_files(None, scope_path);
    let mut remaining = 0usize;
    let skipped = scanned
        .non_utf8
        .iter()
        .map(|(path, count)| {
            remaining += count;
            format!(
                "{}: contains conflict markers but is not valid UTF-8; resolve manually",
                relative_path(path)
            )
        })
        .collect();
    (scanned.files, skipped, remaining)
}

fn collect_conflicts(
    code: &CodeTools,
    scope_path: &str,
) -> (Vec<(String, Vec<ConflictMarker>)>, Vec<String>) {
    let scanned = scan_conflict_files(Some(code), scope_path);
    let mut conflicts = Vec::new();
    for (path, content) in scanned.files {
        let markers = parse_conflicts(&content);
        if !markers.is_empty() {
            let rel = relative_path(&path);
            conflicts.push((rel, markers));
        }
    }
    let warnings = scanned
        .non_utf8
        .iter()
        .map(|(path, _)| {
            format!(
                "{}: contains conflict markers but is not valid UTF-8; resolve manually",
                relative_path(path)
            )
        })
        .collect();
    (conflicts, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn parse_conflicts_finds_single() {
        let content = "\
some code
<<<<<<< HEAD
our change
=======
their change
>>>>>>> feature
more code";
        let markers = parse_conflicts(content);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].start_line, 2);
        assert_eq!(markers[0].end_line, 6);
        assert_eq!(markers[0].our_branch, "HEAD");
        assert_eq!(markers[0].their_branch, "feature");
    }

    #[test]
    fn parse_conflicts_finds_multiple() {
        let content = "\
<<<<<<< a
x
=======
y
>>>>>>> b
code
<<<<<<< c
p
=======
q
>>>>>>> d";
        let markers = parse_conflicts(content);
        assert_eq!(markers.len(), 2);
    }

    #[test]
    fn parse_conflicts_no_markers() {
        let content = "clean file\nno conflicts\n";
        let markers = parse_conflicts(content);
        assert!(markers.is_empty());
    }

    const CONFLICT_TEXT: &str = "\
top
<<<<<<< HEAD
ours-line
=======
theirs-line
>>>>>>> feature
bottom
<<<<<<< HEAD
second-ours
=======
second-theirs
>>>>>>> other
end";

    #[test_case(ConflictSide::Ours, "top\nours-line\nbottom\nsecond-ours\nend" ; "resolve_all_ours")]
    #[test_case(ConflictSide::Theirs, "top\ntheirs-line\nbottom\nsecond-theirs\nend" ; "resolve_all_theirs")]
    #[test_case(ConflictSide::Base, "top\nbottom\nend" ; "resolve_all_base")]
    fn resolve_content_all(side: ConflictSide, expected: &str) {
        let file = resolve_content(CONFLICT_TEXT, side, None);
        assert_eq!(file.resolved, 2);
        assert!(file.unterminated_at.is_none());
        assert_eq!(file.content, expected);
    }

    #[test]
    fn resolve_content_only_nth_keeps_others() {
        let file = resolve_content(CONFLICT_TEXT, ConflictSide::Theirs, Some(2));
        assert_eq!(file.resolved, 1);
        assert!(
            file.content.contains("ours-line"),
            "first conflict should be untouched"
        );
        assert!(file.content.contains("second-theirs"));
    }

    #[test]
    fn resolve_content_only_nth_preserves_original_labels() {
        let file = resolve_content(CONFLICT_TEXT, ConflictSide::Theirs, Some(2));
        assert!(
            file.content
                .contains("<<<<<<< HEAD\nours-line\n=======\ntheirs-line\n>>>>>>> feature\n"),
            "preserved conflict should re-emit the original markers verbatim, got:\n{}",
            file.content
        );
    }

    /// A diff3/zdiff3-style conflict (`merge.conflictStyle = diff3`) carries
    /// a `|||||||` base section that must never leak into a resolution.
    const DIFF3_TEXT: &str = "\
top
<<<<<<< HEAD
ours-line
||||||| merged common ancestors
base-line
=======
theirs-line
>>>>>>> feature
bottom";

    #[test_case(ConflictSide::Ours, "top\nours-line\nbottom" ; "diff3_resolve_ours")]
    #[test_case(ConflictSide::Theirs, "top\ntheirs-line\nbottom" ; "diff3_resolve_theirs")]
    #[test_case(ConflictSide::Base, "top\nbottom" ; "diff3_resolve_base")]
    fn resolve_content_diff3_drops_base_section(side: ConflictSide, expected: &str) {
        let file = resolve_content(DIFF3_TEXT, side, None);
        assert_eq!(file.resolved, 1);
        assert!(file.unterminated_at.is_none());
        assert_eq!(file.content, expected);
    }

    #[test]
    fn resolve_content_diff3_preserved_block_is_verbatim() {
        let file = resolve_content(DIFF3_TEXT, ConflictSide::Ours, Some(2));
        assert_eq!(file.resolved, 0);
        assert_eq!(file.content, DIFF3_TEXT);
    }

    #[test]
    fn parse_conflicts_handles_diff3_sections() {
        let markers = parse_conflicts(DIFF3_TEXT);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].start_line, 2);
        assert_eq!(markers[0].end_line, 8);
        assert_eq!(markers[0].our_branch, "HEAD");
        assert_eq!(markers[0].their_branch, "feature");
    }

    #[test]
    fn resolve_content_aborts_on_unterminated_block() {
        let content = "\
top
<<<<<<< HEAD
ours-line
=======
theirs-line
>>>>>>> feature
bottom
<<<<<<< HEAD
stray-ours
";
        let file = resolve_content(content, ConflictSide::Theirs, None);
        assert_eq!(file.unterminated_at, Some(8), "opener at line 8 never ends");
        assert_eq!(file.resolved, 0);
    }

    #[test]
    fn run_leaves_unterminated_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        let content = "\
top
<<<<<<< HEAD
ours-line
=======
theirs-line
>>>>>>> feature
bottom
<<<<<<< HEAD
stray-ours
";
        std::fs::write(&file, content).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: Some("@theirs".into()),
                index: None,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            content,
            "file with an unterminated block must be left byte-identical"
        );
        assert_eq!(report.resolved, Some(0));
        assert!(
            report.text.contains("no >>>>>>> end marker"),
            "report should name the unterminated block: {}",
            report.text
        );
    }

    #[test]
    fn run_lists_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), CONFLICT_TEXT).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: None,
                index: None,
            },
        )
        .unwrap();
        assert!(report.text.contains("merge conflicts in 1 file(s)"));
        assert!(report.text.contains("HEAD vs feature"));
        assert!(report.resolved.is_none());
    }

    #[test]
    fn run_resolves_theirs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, CONFLICT_TEXT).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: Some("@theirs".into()),
                index: None,
            },
        )
        .unwrap();
        assert_eq!(report.resolved, Some(2));
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "top\ntheirs-line\nbottom\nsecond-theirs\nend");
    }

    #[test]
    fn run_resolves_only_nth() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, CONFLICT_TEXT).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: Some("@ours".into()),
                index: Some(2),
            },
        )
        .unwrap();
        assert_eq!(report.resolved, Some(1));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("theirs-line"), "got {after}");
        assert!(after.contains("second-ours"), "got {after}");
    }

    #[test]
    fn run_rejects_index_zero() {
        let tools = CodeTools::default();
        let err = run(
            &tools,
            ConflictsParams {
                path: None,
                resolve: Some("@ours".into()),
                index: Some(0),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("1-indexed"), "{err}");
    }

    #[test]
    fn resolve_content_handles_crlf_files() {
        let content =
            "top\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> feature\r\nend\r\n";
        let file = resolve_content(content, ConflictSide::Theirs, None);
        assert!(file.unterminated_at.is_none());
        assert_eq!(file.resolved, 1);
        assert_eq!(file.content, "top\r\ntheirs\r\nend\r\n");

        // Preserved (index-mode) blocks keep their CRLF bytes verbatim.
        let kept = resolve_content(content, ConflictSide::Theirs, Some(99));
        assert_eq!(
            kept.content,
            "top\r\n<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> feature\r\nend\r\n",
            "nothing selected, nothing rewritten"
        );
    }

    #[test]
    fn run_rejects_unknown_choice() {
        let tools = CodeTools::default();
        let err = run(
            &tools,
            ConflictsParams {
                path: None,
                resolve: Some("@nope".into()),
                index: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown resolve choice"), "{err}");
    }

    /// A conflicted file that is not valid UTF-8 cannot be resolved here,
    /// but it must not silently vanish from either report.
    #[test]
    fn non_utf8_conflicted_file_is_surfaced_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        // Invalid UTF-8 byte inside otherwise marker-bearing content.
        let mut bytes = b"<<<<<<< HEAD\nours\n".to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"\n=======\ntheirs\n>>>>>>> feature\n");
        std::fs::write(dir.path().join("blob.txt"), bytes).unwrap();

        let tools = CodeTools::default();
        let listed = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: None,
                index: None,
            },
        )
        .unwrap();
        assert!(
            listed.text.contains("not valid UTF-8"),
            "listing must warn: {}",
            listed.text
        );

        let resolved = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: Some("@theirs".into()),
                index: None,
            },
        )
        .unwrap();
        assert_eq!(resolved.resolved, Some(0));
        assert!(
            resolved.text.contains("not valid UTF-8"),
            "resolve must warn: {}",
            resolved.text
        );
        assert!(
            resolved.text.contains("(0 found, 1 remaining)"),
            "the unresolvable conflict must count as remaining: {}",
            resolved.text
        );
    }
}
