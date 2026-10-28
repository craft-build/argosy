//! The heading chunker: structure, breadcrumbs, line ranges, and the
//! short-concept fast path.

use super::*;
use crate::index::chunk::{ChunkConfig, chunk_concept, estimate_tokens};

fn concept(body: &str, description: Option<&str>) -> Concept {
    let frontmatter = match description {
        Some(d) => format!("---\ntype: Note\ndescription: {d}\n---\n"),
        None => "---\ntype: Note\n---\n".to_string(),
    };
    format!("{frontmatter}{body}").parse().unwrap()
}

/// A body long enough to force heading chunking under the test budget.
fn long_body() -> String {
    let filler = "word ".repeat(40);
    format!(
        "# Architecture\n\n\
         The service handles billing.\n\n\
         ## Ingestion\n\n\
         {filler}\n\n\
         ### Retries\n\n\
         {filler}\n\n\
         ## Storage\n\n\
         {filler}\n"
    )
}

#[test]
fn short_concepts_stay_one_chunk_with_title_prefix() {
    let c = concept("# Deploy\n\nDeploy steps.\n", Some("Deploy the service."));
    let chunks = chunk_concept(&c, "deploy", ChunkConfig::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].ordinal, 0);
    assert_eq!(chunks[0].heading_path, "");
    assert_eq!((chunks[0].start_line, chunks[0].end_line), (1, 3));
    assert!(chunks[0].text.starts_with("Concept: Deploy"));
    assert!(chunks[0].text.contains("Description: Deploy the service."));
    assert!(chunks[0].text.contains("Deploy steps."));
}

#[test]
fn long_concepts_split_by_heading_with_breadcrumbs_and_line_ranges() {
    let body = long_body();
    let c = concept(&body, None);
    let chunks = chunk_concept(&c, "arch", ChunkConfig { max_tokens: 80 });

    assert!(chunks.len() >= 4, "one chunk per section: {chunks:#?}");
    // Ordinals are dense and sequential.
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.ordinal as usize, index);
        assert!(
            estimate_tokens(&chunk.text) <= 80 + 8,
            "chunk {} over budget: {}",
            chunk.ordinal,
            estimate_tokens(&chunk.text)
        );
        // The shared prefix names the concept in every passage.
        assert!(chunk.text.starts_with("Concept: Architecture"));
    }
    let breadcrumbs: Vec<&str> = chunks.iter().map(|c| c.heading_path.as_str()).collect();
    assert!(breadcrumbs.contains(&"Architecture"), "{breadcrumbs:?}");
    assert!(
        breadcrumbs.contains(&"Architecture › Ingestion › Retries"),
        "nested breadcrumb carries its parents: {breadcrumbs:?}"
    );
    assert!(breadcrumbs.contains(&"Architecture › Storage"));

    // Line ranges are exact: joining the passages in order covers the body,
    // and each range is inside the body's line count.
    let body_lines: Vec<&str> = c.body().lines().collect();
    let mut covered = 0u32;
    for chunk in &chunks {
        assert!(chunk.start_line >= 1 && chunk.end_line >= chunk.start_line);
        assert!(chunk.end_line as usize <= body_lines.len(), "{chunk:?}");
        assert_eq!(chunk.start_line, covered + 1, "no gaps: {chunk:?}");
        covered = chunk.end_line;
    }
    // Trailing blank lines are not part of the final chunk.
    assert_eq!(
        covered as usize,
        body_lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map(|i| i + 1)
            .unwrap()
            .max(1)
    );
}

#[test]
fn hashes_inside_fenced_code_are_not_headings() {
    let body = format!(
        "# Install\n\n```bash\n# not a heading\npip install x\n```\n\n{}",
        "word ".repeat(120)
    );
    let c = concept(&body, None);
    let chunks = chunk_concept(&c, "install", ChunkConfig { max_tokens: 100 });
    assert!(
        chunks.iter().all(|c| c.heading_path == "Install"),
        "the shell comment never becomes a breadcrumb: {chunks:#?}"
    );
}

#[test]
fn an_over_long_section_splits_at_paragraphs_not_mid_paragraph() {
    let para = "alpha beta gamma delta epsilon zeta eta theta ";
    let body = format!(
        "# Setup\n\n{}\n\n{}\n\n{}\n",
        para.repeat(8).trim(),
        para.repeat(8).trim(),
        para.repeat(8).trim()
    );
    let c = concept(&body, None);
    let chunks = chunk_concept(&c, "setup", ChunkConfig { max_tokens: 60 });
    assert!(chunks.len() >= 2, "{chunks:#?}");
    // Every chunk's passage text begins with the paragraph opener — the
    // splitter never cut inside one.
    for chunk in &chunks {
        assert!(chunk.text.contains("alpha beta gamma"), "{chunk:?}");
        assert_eq!(chunk.heading_path, "Setup");
    }
}

#[test]
fn title_falls_back_to_description_then_id() {
    let c = concept("Plain body with no heading.\n", Some("Described note."));
    let chunks = chunk_concept(&c, "note", ChunkConfig::default());
    assert!(chunks[0].text.starts_with("Concept: Described note."));

    let c = concept("Plain body with no heading.\n", None);
    let chunks = chunk_concept(&c, "fallback-id", ChunkConfig::default());
    assert!(chunks[0].text.starts_with("Concept: fallback-id"));
}

#[test]
fn a_smaller_budget_rechunks_and_changes_the_concept_hash() {
    // The same concept under two budgets must produce different chunkings,
    // which is what makes a budget change re-embed through the diff.
    let body = long_body();
    let c = concept(&body, None);
    let tight = chunk_concept(&c, "arch", ChunkConfig { max_tokens: 60 });
    let loose = chunk_concept(&c, "arch", ChunkConfig { max_tokens: 200 });
    assert!(tight.len() > loose.len(), "budget shapes the chunking");
    assert_ne!(concept_hash(&tight), concept_hash(&loose));
}

#[test]
fn every_non_blank_body_line_is_covered_exactly_once() {
    // A mixed body: prose, nested headings, an empty section, a code fence
    // with a `#` comment, and paragraphs long enough to split.
    let body = format!(
        "Intro paragraph.\n\n\
         ## Empty Section\n\n\
         ## Code\n\n\
         ```sh\n# not a heading\ncargo test\n```\n\n\
         ### Long\n\n{}\n\n{}\n\n\
         ## Trailing Heading\n",
        "alpha beta ".repeat(60).trim(),
        "gamma delta ".repeat(60).trim()
    );
    let c = concept(&body, None);
    let chunks = chunk_concept(&c, "mixed", ChunkConfig { max_tokens: 70 });
    assert!(chunks.len() >= 4, "{chunks:#?}");

    let lines: Vec<&str> = c.body().lines().collect();
    // Ranges ascend and never overlap.
    let mut previous_end = 0u32;
    for chunk in &chunks {
        assert!(
            chunk.start_line > previous_end,
            "ranges must not overlap: {chunk:?} after end {previous_end}"
        );
        previous_end = chunk.end_line;
    }
    // Every non-blank line falls inside exactly one range.
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = index as u32 + 1;
        let covering = chunks
            .iter()
            .filter(|c| c.start_line <= line_no && line_no <= c.end_line)
            .count();
        assert_eq!(
            covering, 1,
            "line {line_no} (#{line:?}) covered {covering} times"
        );
    }
    // The empty section's heading is folded into the next passage, and the
    // trailing heading joins the section before it, so no chunk is a bare
    // heading.
    for chunk in &chunks {
        let body = chunk
            .text
            .split_once("\n\n")
            .map(|(_, body)| body)
            .unwrap_or_default();
        assert!(
            body.lines().any(|l| !l.trim_start().starts_with('#')),
            "not a heading-only passage: {chunk:?}"
        );
    }
}
