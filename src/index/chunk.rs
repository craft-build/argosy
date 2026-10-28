//! Heading-based chunking of concept bodies: the deterministic, no-LLM half
//! of contextual retrieval. A concept whose whole prefixed body fits the
//! token budget stays one chunk; a longer one splits at ATX headings, and
//! any over-long section splits again at paragraph (and, if needed, line)
//! boundaries. Every chunk is embedded behind a `Concept:`/`Section:` prefix
//! — title plus heading breadcrumb — so a passage deep in a long document
//! carries enough context to stand on its own.
//!
//! Chunks are line-exact: `start_line`/`end_line` are 1-based lines of the
//! concept *body* (frontmatter excluded), the same numbering `read` accepts
//! for small-to-big expansion. A `#` inside a fenced code block is never a
//! heading; an over-long fence still splits at line boundaries (an indexed
//! fragment beats an unsearchable tail), and an indivisible line longer than
//! the budget stays whole — the embedder's truncation is the backstop.

use crate::concept::Concept;

use super::is_invisible_format;

/// Token budget for one chunk's embedded text, prefix included. Defaults
/// leave headroom under the embedder's 256-token truncation.
pub const DEFAULT_MAX_TOKENS: usize = 220;

/// The chunking knobs (user configuration `index.chunk_max_tokens`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    /// Maximum estimated tokens per chunk, prefix included.
    pub max_tokens: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

/// One embedded passage of a concept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Position within the source concept, `0..`.
    pub ordinal: u32,
    /// The exact text embedded and returned as the hit's passage: the
    /// `Concept:`/`Section:` prefix, a blank line, then the passage body.
    pub text: String,
    /// The heading breadcrumb (`"Setup › Database"`); empty for the
    /// pre-heading passage and for a whole-concept chunk.
    pub heading_path: String,
    /// 1-based first body line covered (frontmatter excluded).
    pub start_line: u32,
    /// 1-based last body line covered.
    pub end_line: u32,
    /// Invisible format characters stripped from [`Chunk::text`] (see
    /// [`super::is_invisible_format`]); the on-disk bytes are untouched.
    pub invisible: usize,
}

/// Splits a concept into embeddable chunks. `fallback_title` names the
/// concept when neither an H1 nor a frontmatter description provides one
/// (the caller passes the concept id's last segment).
pub fn chunk_concept(concept: &Concept, fallback_title: &str, config: ChunkConfig) -> Vec<Chunk> {
    let body = concept.body();
    let title = concept_title(body, concept.description(), fallback_title);
    let description = concept
        .description()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let body_lines: Vec<&str> = body.lines().collect();
    // Trailing blank lines are not part of any passage: the whole-concept
    // chunk's range must not claim them (a ranged `read` would return air).
    let last_content_line = body_lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|index| index + 1)
        .unwrap_or(1) as u32;
    let prefix = prefix_text(&title, description, "");

    // Short concepts stay whole: one passage, no heading split. This is the
    // common case (skills, rules, memory notes) and keeps their passage
    // identical to the concept body.
    if estimate_tokens(&prefix) + estimate_tokens(body) + 4 <= config.max_tokens {
        return vec![finish_chunk(
            0,
            &title,
            description,
            "",
            body.trim_end(),
            1,
            last_content_line,
        )];
    }

    let mut chunks = Vec::new();
    for section in split_sections(body) {
        let slice = join_lines(&body_lines, section.start, section.end);
        let full = prefix_text(&title, description, &section.heading_path);
        if estimate_tokens(&full) + estimate_tokens(&slice) + 4 <= config.max_tokens {
            chunks.push(finish_chunk(
                chunks.len() as u32,
                &title,
                description,
                &section.heading_path,
                &slice,
                section.start as u32 + 1,
                section.end as u32 + 1,
            ));
            continue;
        }
        // Over-long section: pack whole blocks (paragraphs, code fences,
        // heading lines) greedily, then split an over-budget block at line
        // boundaries.
        let budget = config
            .max_tokens
            .saturating_sub(estimate_tokens(&full) + 4)
            .max(32);
        for (start, end) in pack_blocks(&body_lines, &section, budget) {
            chunks.push(finish_chunk(
                chunks.len() as u32,
                &title,
                description,
                &section.heading_path,
                &join_lines(&body_lines, start, end),
                start as u32 + 1,
                end as u32 + 1,
            ));
        }
    }
    if chunks.is_empty() {
        chunks.push(finish_chunk(
            0,
            &title,
            description,
            "",
            body.trim_end(),
            1,
            last_content_line,
        ));
    }
    chunks
}

/// Approximate token count: whitespace words × 4/3. The chunker cannot see
/// the embedder's tokenizer (it is provider-agnostic), and word counts
/// understate English token length (~1.3 tokens per word), which is why the
/// default budget leaves a margin under the model's 256-token limit.
pub fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    (words * 4).div_ceil(3)
}

/// The concept's display title: its first heading (preferring H1), else the
/// description, else the caller's fallback (the id).
fn concept_title(body: &str, description: Option<&str>, fallback: &str) -> String {
    let mut first_heading: Option<&str> = None;
    let mut in_code = false;
    for line in body.lines() {
        if is_fence(line) {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if let Some((level, text)) = heading(line) {
            if level == 1 {
                return text.to_string();
            }
            first_heading.get_or_insert(text);
            continue;
        }
        // Only the leading heading block counts as a title; prose owns the
        // document after that.
        if !line.trim().is_empty() {
            break;
        }
    }
    if let Some(text) = first_heading {
        return text.to_string();
    }
    if let Some(description) = description.map(str::trim).filter(|d| !d.is_empty()) {
        return description.to_string();
    }
    fallback.to_string()
}

/// The prefix lines shared by every chunk of a concept. The description is
/// capped so a pathological one cannot inflate every passage past the
/// budget (the title and breadcrumb are the load-bearing part).
fn prefix_text(title: &str, description: Option<&str>, breadcrumb: &str) -> String {
    let mut text = format!("Concept: {title}");
    if let Some(description) = description
        && description != title
    {
        text.push_str("\nDescription: ");
        text.push_str(&cap_description(description));
    }
    if !breadcrumb.is_empty() {
        text.push_str(&format!("\nSection: {breadcrumb}"));
    }
    text
}

/// The maximum characters of a frontmatter description copied into a
/// passage prefix.
const MAX_DESCRIPTION_CHARS: usize = 200;

/// Truncates a description to [`MAX_DESCRIPTION_CHARS`] at a char boundary,
/// marking the cut, so the prefix stays small whatever the frontmatter says.
fn cap_description(description: &str) -> String {
    if description.chars().count() <= MAX_DESCRIPTION_CHARS {
        return description.to_string();
    }
    let cut: String = description.chars().take(MAX_DESCRIPTION_CHARS).collect();
    let cut = cut.trim_end();
    format!("{cut}…")
}

fn finish_chunk(
    ordinal: u32,
    title: &str,
    description: Option<&str>,
    breadcrumb: &str,
    slice: &str,
    start_line: u32,
    end_line: u32,
) -> Chunk {
    let raw = format!(
        "{}\n\n{}",
        prefix_text(title, description, breadcrumb),
        slice.trim_end()
    );
    let (text, invisible) = strip_invisible(&raw);
    Chunk {
        ordinal,
        text,
        heading_path: breadcrumb.to_string(),
        start_line,
        end_line,
        invisible,
    }
}

/// Removes invisible format characters from the indexed text, counting
/// them. Only the embedded text changes, never the bytes on disk.
fn strip_invisible(raw: &str) -> (String, usize) {
    if !raw.contains(is_invisible_format) {
        return (raw.to_string(), 0);
    }
    let mut stripped = 0usize;
    let mut text = String::with_capacity(raw.len());
    for c in raw.chars() {
        if is_invisible_format(c) {
            stripped += 1;
        } else {
            text.push(c);
        }
    }
    (text, stripped)
}

/// One contiguous section of a body: a heading and everything beneath it
/// until the next heading of any level (the pre-heading prose is its own
/// section with an empty breadcrumb).
struct Section {
    heading_path: String,
    /// 0-based inclusive line range.
    start: usize,
    end: usize,
}

/// Splits the body at ATX headings, tracking a heading stack so each
/// section knows its breadcrumb. Fenced code is opaque — a `#` inside a
/// fence is code, not a heading.
fn split_sections(body: &str) -> Vec<Section> {
    let lines: Vec<&str> = body.lines().collect();
    let mut sections = Vec::new();
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut in_code = false;
    let mut current: (String, usize) = (String::new(), 0);

    for (index, line) in lines.iter().enumerate() {
        if is_fence(line) {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let Some((level, text)) = heading(line) else {
            continue;
        };
        if index > current.1 {
            sections.push(Section {
                heading_path: current.0.clone(),
                start: current.1,
                end: index - 1,
            });
        }
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            stack.pop();
        }
        stack.push((level, text.to_string()));
        let breadcrumb = stack
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" › ");
        current = (breadcrumb, index);
    }
    if current.1 < lines.len() {
        sections.push(Section {
            heading_path: current.0,
            start: current.1,
            end: lines.len() - 1,
        });
    }
    sections.retain(|s| !join_lines(&lines, s.start, s.end).trim().is_empty());
    merge_heading_only_sections(&lines, sections)
}

/// A heading with no content beneath it (adjacent headings, or a trailing
/// empty section) must not become its own passage: attach its lines to the
/// following section (the last one joins its predecessor), keeping coverage
/// gapless while the *content's* breadcrumb wins.
fn merge_heading_only_sections(lines: &[&str], sections: Vec<Section>) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::with_capacity(sections.len());
    let mut pending: Option<Section> = None;
    for section in sections {
        let start = pending.take().map_or(section.start, |p| p.start);
        if only_headings(lines, start, section.end) {
            pending = Some(Section {
                heading_path: section.heading_path,
                start,
                end: section.end,
            });
            continue;
        }
        out.push(Section {
            heading_path: section.heading_path,
            start,
            end: section.end,
        });
    }
    if let Some(pending) = pending {
        // A trailing heading joins its predecessor, blank separator included
        // (a range may cover blank lines; they carry no content).
        match out.last_mut() {
            Some(last) if last.end < pending.start => last.end = pending.end,
            _ => out.push(pending),
        }
    }
    out
}

/// A section's blocks: runs of non-blank lines (paragraphs and heading
/// lines), with fenced code blocks kept whole.
type Block = (usize, usize);

fn section_blocks(lines: &[&str], start: usize, end: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        if lines[cursor].trim().is_empty() {
            cursor += 1;
            continue;
        }
        let block_start = cursor;
        let mut in_code = is_fence(lines[cursor]);
        let mut block_end = cursor;
        while block_end < end {
            let next = lines[block_end + 1];
            if in_code {
                block_end += 1;
                if is_fence(next) {
                    in_code = false;
                }
                continue;
            }
            // A blank line ends the block; it is never part of the range, so
            // a heading-only block is exactly one line.
            if next.trim().is_empty() {
                break;
            }
            block_end += 1;
            if is_fence(next) {
                in_code = true;
            }
        }
        blocks.push((block_start, block_end));
        cursor = block_end + 1;
    }
    // A section's leading heading line is a block of its own; it belongs
    // with the passage beneath it rather than becoming a one-line chunk (its
    // text is already in the prefix breadcrumb).
    merge_heading_blocks(lines, blocks)
}

/// Attaches a heading-only block to the block that follows it (a trailing one
/// joins its predecessor), so headings never stand as passages alone.
fn merge_heading_blocks(lines: &[&str], blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    let mut pending: Option<usize> = None;
    for block in blocks {
        let start = pending.take().unwrap_or(block.0);
        if block.0 == block.1 && heading(lines[block.0]).is_some() {
            pending = Some(start);
            continue;
        }
        out.push((start, block.1));
    }
    if let Some(start) = pending {
        // A trailing heading joins its predecessor, blank separator included
        // (the range may cover blank lines; they carry no content).
        match out.last_mut() {
            Some(last) if last.1 < start => last.1 = start,
            _ => out.push((start, start)),
        }
    }
    out
}

/// Packs a section's blocks into line ranges under `budget`, splitting any
/// block that alone exceeds it.
fn pack_blocks(lines: &[&str], section: &Section, budget: usize) -> Vec<Block> {
    let mut packed = Vec::new();
    let mut cursor: Option<Block> = None;
    for block in section_blocks(lines, section.start, section.end) {
        let block_text = join_lines(lines, block.0, block.1);
        if estimate_tokens(&block_text) > budget {
            if let Some(done) = cursor.take() {
                packed.push(done);
            }
            packed.extend(split_block(lines, block, budget));
            continue;
        }
        match cursor {
            Some((start, _)) if estimate_tokens(&join_lines(lines, start, block.1)) <= budget => {
                cursor = Some((start, block.1));
            }
            _ => {
                if let Some(done) = cursor.take() {
                    packed.push(done);
                }
                cursor = Some(block);
            }
        }
    }
    if let Some(done) = cursor {
        packed.push(done);
    }
    packed
}

/// Splits an over-budget block at line boundaries. A single over-budget
/// line stays whole: its line range stays exact and the embedder's
/// truncation is the backstop. A leading run of heading lines stays attached
/// to the first piece rather than becoming a one-line passage.
fn split_block(lines: &[&str], block: Block, budget: usize) -> Vec<Block> {
    let mut out = Vec::new();
    let mut start = block.0;
    for cursor in block.0 + 1..=block.1 {
        if estimate_tokens(&join_lines(lines, start, cursor)) > budget
            && !only_headings(lines, start, cursor - 1)
        {
            out.push((start, cursor - 1));
            start = cursor;
        }
    }
    out.push((start, block.1));
    out
}

/// True iff every non-blank line in the range is a heading — such a run is a
/// section's title, never a passage of its own.
fn only_headings(lines: &[&str], start: usize, end: usize) -> bool {
    lines[start..=end]
        .iter()
        .all(|line| line.trim().is_empty() || heading(line).is_some())
}

/// Joins a 0-based inclusive line range of the body.
fn join_lines(lines: &[&str], start: usize, end: usize) -> String {
    let end = end.min(lines.len().saturating_sub(1));
    lines[start.min(end)..=end].join("\n")
}

/// An ATX heading: 1–6 `#` after at most three spaces of indent, then a
/// space (or the end of the line). Setext headings are not recognized, and
/// the trailing closing sequence (`## Title ##`) is trimmed.
fn heading(line: &str) -> Option<(u8, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !after.is_empty() && !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let mut text = after.trim();
    // Trim a CommonMark closing sequence: `## Title ##`.
    if text.ends_with('#') {
        let trimmed = text.trim_end_matches('#');
        if trimmed.is_empty() || trimmed.ends_with(' ') || trimmed.ends_with('\t') {
            text = trimmed.trim_end();
        }
    }
    if text.is_empty() {
        return None;
    }
    Some((level as u8, text))
}

fn is_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}
