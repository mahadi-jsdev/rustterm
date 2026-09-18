const MODEL: &str = "gpt-4o-mini";
const MAX_DIFF_CHARS: usize = 8_000;

/// Generate a conventional-commit subject line for a staged diff.
/// Runs synchronously — callers must spawn it on a worker thread.
pub fn generate_message(diff: &str, api_key: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [{"role": "user", "content": prompt(diff)}],
        "max_tokens": 60,
        "temperature": 0.2,
    });
    let resp = ureq::post("https://api.openai.com/v1/chat/completions")
        .timeout(std::time::Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {api_key}"))
        .send_json(body)
        .map_err(|e| format!("openai: {e}"))?;
    let text = resp.into_string().map_err(|e| format!("openai read: {e}"))?;
    parse_response(&text)
}

/// Extract the trimmed message content; Err on empty/missing.
fn parse_response(body: &str) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("openai json: {e}"))?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "openai: empty response".to_string())
}

/// Conventional-commit prompt; diff truncated to MAX_DIFF_CHARS on a
/// char boundary so multi-byte content never panics.
fn prompt(diff: &str) -> String {
    let truncated: String = diff.chars().take(MAX_DIFF_CHARS).collect();
    format!(
        "Write a conventional commit message subject line (type: summary, \
         \u{2264}72 chars, imperative, no body, no quotes) for this staged diff. \
         Output ONLY the subject line.\n\n{truncated}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_contains_instructions_and_the_diff() {
        let p = prompt("diff --git a/x b/x");
        assert!(p.contains("conventional commit"));
        assert!(p.contains("diff --git a/x b/x"));
        assert!(p.contains("72"));
    }

    #[test]
    fn prompt_truncates_huge_diffs_on_char_boundary() {
        let big = "é".repeat(20_000); // multi-byte: must not split mid-char
        let p = prompt(&big);
        assert!(p.len() < 20_000);
        assert!(p.len() > 8_000); // instructions + ~8k of diff
    }

    #[test]
    fn parse_response_extracts_trimmed_content() {
        let body = r#"{"choices":[{"message":{"content":"  feat: add thing\n"}}]}"#;
        assert_eq!(parse_response(body).unwrap(), "feat: add thing");
    }

    #[test]
    fn parse_response_rejects_empty_or_missing_content() {
        assert!(parse_response(r#"{"choices":[]}"#).is_err());
        assert!(parse_response(r#"{"choices":[{"message":{"content":"  "}}]}"#).is_err());
    }
}
