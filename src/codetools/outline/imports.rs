//! Import-statement handling: parsing `use`/`import` segments into paths
//! and folding them into a trie for compact rendering.

pub(super) fn parse_import_segments(sig: &str, sep: &str) -> Vec<Vec<String>> {
    let cleaned = sig
        .trim()
        .trim_end_matches(';')
        .trim_start_matches("use ")
        .trim_start_matches("pub use ")
        .trim_start_matches("import ")
        .trim_start_matches("from ");
    expand_import_paths(cleaned, sep)
}

pub(super) fn expand_import_paths(text: &str, sep: &str) -> Vec<Vec<String>> {
    let mut results = Vec::new();
    let mut stack: Vec<(Vec<String>, &str)> = vec![(Vec::new(), text.trim())];

    while let Some((prefix, remaining)) = stack.pop() {
        let remaining = remaining.trim();
        if remaining.is_empty() {
            if !prefix.is_empty() {
                results.push(prefix);
            }
            continue;
        }

        if let Some(pos) = find_sep_top_level(remaining, sep) {
            let segment = remaining[..pos].trim();
            let rest = remaining[pos + sep.len()..].trim();
            let mut new_prefix = prefix.clone();
            new_prefix.push(segment.to_string());

            if let Some(inner) = strip_braces(rest) {
                for item in split_top_level(inner, ',').into_iter().rev() {
                    let cp = new_prefix.clone();
                    stack.push((cp, item));
                }
            } else {
                stack.push((new_prefix, rest));
            }
        } else {
            let mut path = prefix;
            path.push(remaining.to_string());
            results.push(path);
        }
    }

    results
}

fn find_sep_top_level(text: &str, sep: &str) -> Option<usize> {
    let mut depth = 0usize;
    let sep_bytes = sep.as_bytes();
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'{' | b'(' => depth += 1,
            b'}' | b')' => {
                depth = depth.saturating_sub(1);
            }
            _ if depth == 0
                && i + sep_bytes.len() <= bytes.len()
                && &bytes[i..i + sep_bytes.len()] == sep_bytes =>
            {
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

fn strip_braces(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.starts_with('{') && t.ends_with('}') {
        Some(&t[1..t.len() - 1])
    } else {
        None
    }
}

fn split_top_level(text: &str, delim: char) -> Vec<&str> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut results = Vec::new();
    for (i, c) in text.char_indices() {
        match c {
            '{' | '(' => depth += 1,
            '}' | ')' => {
                depth = depth.saturating_sub(1);
            }
            _ if c == delim && depth == 0 => {
                results.push(text[start..i].trim());
                start = i + delim.len_utf8();
            }
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        results.push(last);
    }
    results
}

pub(super) struct TrieNode {
    children: std::collections::BTreeMap<String, TrieNode>,
    is_leaf: bool,
}

impl TrieNode {
    pub(super) fn new() -> Self {
        Self {
            children: std::collections::BTreeMap::new(),
            is_leaf: false,
        }
    }

    pub(super) fn insert(&mut self, segments: &[String]) {
        let mut node = self;
        for seg in segments {
            node = node
                .children
                .entry(seg.clone())
                .or_insert_with(TrieNode::new);
        }
        node.is_leaf = true;
    }
}

pub(super) fn render_trie(node: &TrieNode, sep: &str) -> Vec<String> {
    let mut result = Vec::new();
    for (seg, child) in &node.children {
        let rendered = render_trie(child, sep);
        if rendered.is_empty() {
            result.push(seg.clone());
        } else if child.is_leaf {
            result.push(format!("{seg}{sep}{}", rendered.join(", ")));
            result.push(seg.clone());
        } else if rendered.len() == 1 {
            result.push(format!("{seg}{sep}{}", rendered[0]));
        } else {
            result.push(format!("{seg}{sep}{{{}}}", rendered.join(", ")));
        }
    }
    result
}
