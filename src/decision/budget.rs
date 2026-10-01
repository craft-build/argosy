//! Fitting decision requests to an endpoint's context window.
//!
//! The Jev family runs small context windows — laya's shipped checkpoints
//! read 512 tokens (`english`), 1024 (`multilingual`, `typed-decisions`),
//! or up to 8192 (`laya-multilingual` with `max_len=8192`) — and an
//! over-budget request is either rejected or silently cut, potentially
//! past the questions it carries. argosy cannot know the serving
//! checkpoint's exact limit, so it fits each request to a configurable
//! token *estimate* before sending:
//! [`crate::config::DecisionConfig::max_input_tokens`].
//!
//! Token counts are a heuristic, not a tokenizer: roughly 3 characters per
//! token for Latin-script text (JSON, code, and URIs tokenize denser than
//! prose) and one per CJK character, whitespace-free. Over-budget requests
//! are trimmed longest-field-first — the bulk carriers (a question, the
//! code under review, a diff hunk, rule examples) shed tokens before
//! anything short is touched — and every cut is marked in place so the
//! endpoint can see that content was dropped. Object keys (question names,
//! option labels) are counted but never rewritten: answers map back onto
//! them, so trimming a key would corrupt the response. Fitting never
//! fails; when the untrimmable overhead alone exceeds the budget the
//! request still goes out, matching the decision layer's fail-open
//! philosophy.

/// Conservative characters-per-token for Latin-script payload text. The
/// repomap renderer uses 4 for prose; decision payloads mix in JSON
/// syntax, code, and URIs, which tokenize denser.
const CHARS_PER_TOKEN: usize = 3;

/// Fields at or below this many estimated tokens are never trimmed: the
/// in-place cut marker costs a handful of tokens itself, so shrinking a
/// short field gains nothing.
const MIN_FIELD_TOKENS: usize = 16;

/// Whether a character tokenizes at roughly one token per character
/// (CJK scripts and fullwidth punctuation on laya's multilingual
/// checkpoint).
fn is_dense(ch: char) -> bool {
    matches!(
        ch,
        '\u{3000}'..='\u{303F}'   // CJK symbols and punctuation
            | '\u{3040}'..='\u{30FF}' // hiragana, katakana
            | '\u{3400}'..='\u{4DBF}' // CJK unified ext A
            | '\u{4E00}'..='\u{9FFF}' // CJK unified ideographs
            | '\u{AC00}'..='\u{D7AF}' // hangul syllables
            | '\u{F900}'..='\u{FAFF}' // CJK compatibility ideographs
            | '\u{FF00}'..='\u{FFEF}' // fullwidth and halfwidth forms
            | '\u{20000}'..='\u{2A6DF}' // CJK unified ext B
    )
}

/// Estimated token count of `text`: dense-script characters count one
/// each, everything else roughly `CHARS_PER_TOKEN` per character,
/// whitespace-free.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    let mut dense = 0usize;
    let mut plain = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if is_dense(ch) {
            dense += 1;
        } else {
            plain += 1;
        }
    }
    dense + plain.div_ceil(CHARS_PER_TOKEN)
}

/// `text` with head and tail kept and the middle replaced by a marker
/// stating how many characters were cut. Head-heavy (60/40) because the
/// start of a state usually carries more signal than its end.
fn head_tail(text: &str, allowance_chars: usize) -> String {
    let total = text.chars().count();
    if total <= allowance_chars {
        return text.to_string();
    }
    let head = (allowance_chars * 3) / 5;
    let tail = allowance_chars.saturating_sub(head).min(total - head);
    let cut = total - head - tail;
    let head_str: String = text.chars().take(head).collect();
    let tail_str: String = text.chars().skip(total - tail).collect();
    format!("{head_str} […{cut} chars cut…] {tail_str}")
}

/// Trims `text` to at most `max_tokens` (estimated), keeping its head and
/// tail and marking the cut. Text that already fits is returned unchanged.
pub(crate) fn fit_text(text: &str, max_tokens: usize) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    // The marker costs tokens too, and dense scripts break the
    // chars-per-token arithmetic, so propose a character allowance and
    // shrink it until the estimate actually fits.
    let mut allowance = max_tokens.saturating_mul(CHARS_PER_TOKEN);
    while allowance > 0 {
        let candidate = head_tail(text, allowance);
        if estimate_tokens(&candidate) <= max_tokens {
            return candidate;
        }
        allowance = (allowance * 9) / 10;
    }
    // Degenerate budget: a plain head cut of one character per token
    // always fits the estimate.
    text.chars().take(max_tokens).collect()
}

/// Estimated input tokens of a whole request: every string value plus the
/// untrimmable overhead (object keys, question names, model passthrough).
pub(crate) fn request_tokens(request: &super::DecisionRequest) -> usize {
    let mut overhead = request
        .model
        .as_ref()
        .map_or(0, |model| estimate_tokens(model));
    let mut slots: Vec<&String> = Vec::new();
    collect_string_refs(&request.state, &mut overhead, &mut slots);
    for (name, question) in &request.questions {
        overhead += estimate_tokens(name);
        collect_string_refs(question, &mut overhead, &mut slots);
    }
    overhead + slots.iter().map(|s| estimate_tokens(s)).sum::<usize>()
}

/// Collects every string *value* under `value` for trimming. Object keys
/// are skipped: they are counted in [`request_tokens`] but must never be
/// rewritten (answers map back onto question names and option labels).
fn collect_strings<'a>(value: &'a mut serde_json::Value, slots: &mut Vec<&'a mut String>) {
    match value {
        serde_json::Value::String(slot) => slots.push(slot),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, slots);
            }
        }
        serde_json::Value::Object(map) => {
            for (_key, item) in map {
                collect_strings(item, slots);
            }
        }
        _ => {}
    }
}

/// The immutable twin of [`collect_strings`], for estimation: also sums
/// the estimated cost of object keys and question names into `overhead`.
fn collect_string_refs<'a>(
    value: &'a serde_json::Value,
    overhead: &mut usize,
    slots: &mut Vec<&'a String>,
) {
    match value {
        serde_json::Value::String(slot) => slots.push(slot),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_string_refs(item, overhead, slots);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                *overhead += estimate_tokens(key);
                collect_string_refs(item, overhead, slots);
            }
        }
        _ => {}
    }
}

/// Fits a request to at most `budget` estimated input tokens by trimming
/// its longest string values (state fields, question instructions, option
/// descriptions) first, marking each cut in place. Keys, labels, and the
/// model passthrough are counted but left intact. A request whose
/// untrimmable parts alone exceed the budget is still returned — fitting
/// is best-effort, never a gate.
pub(crate) fn fit_request(request: &mut super::DecisionRequest, budget: usize) {
    let total = request_tokens(request);
    if total <= budget {
        return;
    }
    let mut slots: Vec<&mut String> = Vec::new();
    collect_strings(&mut request.state, &mut slots);
    for (_name, question) in &mut request.questions {
        collect_strings(question, &mut slots);
    }

    let mut over = total - budget;
    // Longest first: the bulk fields shed tokens before anything short is
    // touched, and a single oversized field is trimmed alone when it can
    // cover the overflow.
    slots.sort_by_key(|slot| std::cmp::Reverse(estimate_tokens(slot.as_str())));
    for slot in slots {
        if over == 0 {
            break;
        }
        let est = estimate_tokens(slot.as_str());
        let keep = est.saturating_sub(over).max(MIN_FIELD_TOKENS.min(est));
        if keep >= est {
            continue;
        }
        let fitted = fit_text(slot.as_str(), keep);
        over = over.saturating_sub(est - estimate_tokens(&fitted));
        *slot = fitted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_counts_dense_scripts_one_per_character() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hello"), 2); // 5 chars / 3
        assert_eq!(estimate_tokens("こんにちは世界"), 7); // one each
        // Whitespace is free; mixed text sums both regimes.
        assert_eq!(estimate_tokens("hi こんにちは"), 1 + 5);
    }

    #[test]
    fn fit_text_returns_fitting_text_unchanged() {
        let text = "short text";
        assert_eq!(fit_text(text, 100), text);
    }

    #[test]
    fn fit_text_keeps_head_and_tail_and_marks_the_cut() {
        let text: String = (0..500).map(|i| format!("word{i} ")).collect();
        let fitted = fit_text(&text, 50);
        assert!(fitted.contains("chars cut…"));
        assert!(fitted.starts_with("word0"));
        assert!(fitted.ends_with("word499 "));
        assert!(estimate_tokens(&fitted) <= 50);
    }

    #[test]
    fn fit_text_is_char_boundary_safe() {
        // Multibyte-heavy text must never panic on a slice boundary.
        let text: String = "こんにちは🌍🎉".repeat(200);
        let fitted = fit_text(&text, 30);
        assert!(estimate_tokens(&fitted) <= 30);
    }

    #[test]
    fn fit_text_survives_a_degenerate_budget() {
        let text = "a fairly long run of ordinary words".repeat(20);
        let fitted = fit_text(&text, 1);
        assert!(estimate_tokens(&fitted) <= 1);
    }

    fn sample_request() -> super::super::DecisionRequest {
        super::super::DecisionRequest::new(serde_json::json!({
            "code_under_review": "fn main() { println!(\"hi\"); }",
            "candidate_rules": {"rule/a": "prefer explicit types"},
        }))
        .ask(
            "rule_0",
            super::super::Question::noul("Does rule `rule/a` apply to the code under review?"),
        )
    }

    #[test]
    fn fit_request_is_a_noop_within_budget() {
        let mut request = sample_request();
        let before = serde_json::to_value(&request).unwrap();
        fit_request(&mut request, 4_096);
        assert_eq!(serde_json::to_value(&request).unwrap(), before);
    }

    #[test]
    fn fit_request_trims_the_longest_field_first_and_only() {
        let mut request = sample_request();
        // Same request, but the code field is now enormous.
        request.state["code_under_review"] =
            serde_json::Value::String("let x = 1;\n".repeat(2_000));
        fit_request(&mut request, 120);
        let code = request.state["code_under_review"].as_str().unwrap();
        assert!(code.contains("chars cut…"), "the oversized field is marked");
        assert!(code.contains("let x = 1;"));
        // The short fields and the question are untouched.
        assert_eq!(
            request.state["candidate_rules"]["rule/a"].as_str(),
            Some("prefer explicit types")
        );
        assert_eq!(
            request.questions["rule_0"]["instructions"].as_str(),
            Some("Does rule `rule/a` apply to the code under review?")
        );
    }

    #[test]
    fn fit_request_never_rewrites_keys_or_labels() {
        let mut request = super::super::DecisionRequest::new(
            serde_json::json!({"question": "how do I retire a worker?".repeat(50)}),
        )
        .ask(
            "answer",
            super::super::Question::choice(
                "Which document answers the question?",
                [
                    (
                        "A",
                        "argosy://local/document/workers — long description ".repeat(50),
                    ),
                    (
                        "NONE",
                        "none of the documents answer the question".to_string(),
                    ),
                ],
            ),
        );
        fit_request(&mut request, 60);
        let question = &request.questions["answer"];
        // Labels (object keys) survive verbatim; only values are trimmed.
        assert!(question["criteria"].as_object().unwrap().contains_key("A"));
        assert!(
            question["criteria"]
                .as_object()
                .unwrap()
                .contains_key("NONE")
        );
        assert!(
            question["criteria"]["A"]
                .as_str()
                .unwrap()
                .contains("chars cut…")
        );
        assert_eq!(
            question["instructions"].as_str(),
            Some("Which document answers the question?")
        );
    }

    #[test]
    fn fit_request_spills_onto_the_next_longest_field() {
        let mut request = super::super::DecisionRequest::new(serde_json::json!({
            "code": "x".repeat(900),
            "body": "y".repeat(900),
        }));
        fit_request(&mut request, 200);
        let code = request.state["code"].as_str().unwrap();
        let body = request.state["body"].as_str().unwrap();
        // Both bulk fields are trimmed, each marked.
        assert!(code.contains("chars cut…"));
        assert!(body.contains("chars cut…"));
    }

    #[test]
    fn fit_request_never_panics_when_overhead_alone_exceeds_the_budget() {
        let mut request = sample_request();
        fit_request(&mut request, 1);
        // Still serializable, keys intact.
        assert!(serde_json::to_value(&request).is_ok());
        assert!(request.questions.contains_key("rule_0"));
    }
}
