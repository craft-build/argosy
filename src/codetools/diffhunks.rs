//! Unified-diff structure: each file's added lines with their new-file
//! line numbers, and the new-file line ranges its hunks cover.
//!
//! Both the deterministic `pattern` pre-pass (match added lines) and
//! `report_finding` grounding (findings must sit inside a hunk's new-line
//! range) walk this view of the snapshotted diff.

/// One changed file's hunk structure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileHunks {
    /// Repository-relative path, the `b/` side of the `diff --git` header —
    /// the same spelling `changed_files` uses.
    pub path: String,
    /// Inclusive new-file line ranges, one per hunk. A hunk that removes
    /// every line (new count 0) contributes none: no new-file line exists
    /// to point a finding at.
    pub new_ranges: Vec<(u32, u32)>,
    /// Each added (`+`) line with its 1-based new-file line number, in
    /// diff order.
    pub added: Vec<(u32, String)>,
}

/// Parses a unified diff into per-file hunk structure, in diff order.
///
/// Header lines between `diff --git` and the first `@@` (index, modes,
/// renames, `---`/`+++`) are skipped; `\ No newline` markers and removed
/// lines never produce new-file line numbers.
pub(crate) fn parse(diff: &str) -> Vec<FileHunks> {
    let mut files: Vec<FileHunks> = Vec::new();
    // New-file line number the next context/added line will take.
    let mut cursor_new: u32 = 0;
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(path) = diff_header_path(line) {
            files.push(FileHunks {
                path,
                new_ranges: Vec::new(),
                added: Vec::new(),
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if let Some((start, count)) = parse_hunk_header(line) {
            if count > 0 {
                file.new_ranges.push((start, start + count - 1));
            }
            cursor_new = start;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        // An empty diff line is an empty context line git emitted without
        // its leading space; anything else carries the sigil verbatim.
        match line.chars().next() {
            Some('+') => {
                file.added.push((cursor_new, line[1..].to_string()));
                cursor_new += 1;
            }
            Some('-') | Some('\\') => {}
            Some(' ') | None => {
                cursor_new += 1;
            }
            // A stray line inside a hunk cannot occur in git output; treat
            // it as context so line accounting never silently drifts.
            _ => cursor_new += 1,
        }
    }
    files
}

/// The repository-relative path of one `diff --git` header line (its `b/`
/// side), or `None` for any other line.
pub(crate) fn diff_header_path(line: &str) -> Option<String> {
    line.strip_prefix("diff --git ")?
        .rsplit_once(" b/")
        .map(|(_, value)| value.trim_end_matches('"').to_string())
}

/// The new-file `(start, count)` of one `@@ -a[,b] +c[,d] @@` header line;
/// an omitted count means 1. `None` for any other line.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // The segment between the first and second `@@` holds the range specs.
    let rest = line.strip_prefix("@@")?.split("@@").next()?;
    rest.split_whitespace()
        .find_map(|token| token.strip_prefix('+').map(parse_counts))
}

/// `l` → `(l, 1)`; `l,s` → `(l, s)`.
fn parse_counts(spec: &str) -> (u32, u32) {
    match spec.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(0)),
        None => (spec.parse().unwrap_or(0), 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(files: &[FileHunks]) -> Vec<&str> {
        files.iter().map(|f| f.path.as_str()).collect()
    }

    fn added_of<'a>(files: &'a [FileHunks], path: &str) -> &'a [(u32, String)] {
        &files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no hunks for `{path}`"))
            .added
    }

    #[test]
    fn added_lines_carry_new_file_numbers_around_removals() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index 111..222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -2,4 +2,5 @@ fn one() {
 context
-removed
+first added
+second added
 context
+third added
";
        let files = parse(diff);
        assert_eq!(paths(&files), vec!["src/lib.rs"]);
        assert_eq!(files[0].new_ranges, vec![(2, 6)]);
        assert_eq!(
            files[0].added,
            vec![
                (3u32, "first added".to_string()),
                (4u32, "second added".to_string()),
                (6u32, "third added".to_string()),
            ]
        );
    }

    #[test]
    fn multiple_hunks_keep_disjoint_new_ranges() {
        let diff = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
-old one
+new one
@@ -10 +10,2 @@
-old ten
+new ten
+new eleven
";
        let files = parse(diff);
        assert_eq!(files[0].new_ranges, vec![(1, 2), (10, 11)]);
        assert_eq!(
            files[0].added,
            vec![
                (1u32, "new one".to_string()),
                (10u32, "new ten".to_string()),
                (11u32, "new eleven".to_string()),
            ]
        );
    }

    #[test]
    fn file_creation_counts_from_line_one_and_deletion_has_no_new_lines() {
        let diff = "\
diff --git a/made.txt b/made.txt
new file mode 100644
--- /dev/null
+++ b/made.txt
@@ -0,0 +1,2 @@
+one
+two
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-content
";
        let files = parse(diff);
        assert_eq!(paths(&files), vec!["made.txt", "gone.txt"]);
        assert_eq!(files[0].new_ranges, vec![(1, 2)]);
        assert_eq!(added_of(&files, "made.txt").len(), 2);
        assert!(
            files[1].new_ranges.is_empty(),
            "a deleted file has no new-file lines"
        );
        assert!(files[1].added.is_empty());
    }

    #[test]
    fn rename_headers_and_no_newline_markers_do_not_desync_numbers() {
        let diff = "\
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1,2 +1,2 @@
-old
+new
\\ No newline at end of file
 context
";
        let files = parse(diff);
        assert_eq!(paths(&files), vec!["new.rs"]);
        assert_eq!(files[0].new_ranges, vec![(1, 2)]);
        assert_eq!(files[0].added, vec![(1u32, "new".to_string())]);
    }

    #[test]
    fn a_content_line_starting_with_plus_is_one_added_line() {
        // `+++` in the body is content whose first char is `+`, not a header.
        let diff = "\
diff --git a/notes.txt b/notes.txt
--- a/notes.txt
+++ b/notes.txt
@@ -1 +1 @@
+++++
";
        let files = parse(diff);
        assert_eq!(added_of(&files, "notes.txt"), &[(1u32, "++++".to_string())]);
    }
}
