//! AI command assistance: builds Anthropic Messages API requests and parses
//! responses, GUI- and HTTP-client-independent so it can be unit tested.
//!
//! Privacy-first by design: the request carries ONLY the user's typed query
//! plus coarse environment hints (OS name, shell name). Terminal buffer
//! contents, command history, and file contents are never included. The
//! user's own API key is used; nothing is proxied through third parties.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Default model for command suggestions.
pub const DEFAULT_AI_MODEL: &str = "claude-opus-5";
pub const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiSuggestion {
    /// The suggested shell command. Never executed automatically — the UI
    /// only offers insert/copy so the user reviews it first.
    pub command: String,
    pub explanation: String,
    /// Non-empty only when the command is destructive or risky.
    #[serde(default)]
    pub caution: String,
}

/// Builds the Messages API request body. Structured outputs
/// (`output_config.format`) guarantee a parseable JSON answer, and thinking
/// is left at the model's default (on current models it is adaptive).
pub fn build_request(query: &str, os: &str, shell: &str, model: &str) -> Value {
    let model = if model.trim().is_empty() { DEFAULT_AI_MODEL } else { model.trim() };
    let system = format!(
        "You are a command-line assistant embedded in a terminal application. \
         The user describes what they want to do; reply with one shell command \
         that accomplishes it in their environment (OS: {os}, shell: {shell}). \
         Prefer safe, widely available tools. If the command is destructive or \
         risky (deletes data, overwrites files, changes system state), say so \
         briefly in `caution`; otherwise leave `caution` empty. Write \
         `explanation` and `caution` in the same language the user wrote in."
    );
    json!({
        "model": model,
        "max_tokens": 16000,
        "system": system,
        "messages": [{ "role": "user", "content": query }],
        "output_config": {
            "format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "The shell command to run" },
                        "explanation": { "type": "string", "description": "One or two sentences on what it does" },
                        "caution": { "type": "string", "description": "Warning if destructive/risky, else empty string" }
                    },
                    "required": ["command", "explanation", "caution"],
                    "additionalProperties": false
                }
            }
        }
    })
}

/// Parses a Messages API response body into a suggestion. Checks
/// `stop_reason` before touching `content`: safety classifiers can decline a
/// request with a normal HTTP 200 and `stop_reason: "refusal"`, and a
/// truncated (`max_tokens`) answer may not be valid JSON.
pub fn parse_response(body: &str) -> Result<AiSuggestion, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("invalid response: {e}"))?;
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("unknown API error");
        return Err(format!("API error: {msg}"));
    }
    match v.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => return Err("refusal".into()),
        Some("max_tokens") => return Err("response truncated (max_tokens)".into()),
        _ => {}
    }
    let text = v
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks.iter().find_map(|b| {
                (b.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| b.get("text").and_then(Value::as_str))
                    .flatten()
            })
        })
        .ok_or_else(|| "no text content in response".to_string())?;
    let s: AiSuggestion =
        serde_json::from_str(text).map_err(|e| format!("unexpected answer format: {e}"))?;
    if s.command.trim().is_empty() {
        return Err("empty command in answer".into());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_carries_only_query_and_env() {
        let req = build_request("list files over 100MB", "linux", "bash", "");
        assert_eq!(req["model"], DEFAULT_AI_MODEL);
        assert_eq!(req["messages"][0]["content"], "list files over 100MB");
        let system = req["system"].as_str().unwrap();
        assert!(system.contains("OS: linux"));
        assert!(system.contains("shell: bash"));
        // Structured output schema is strict so the answer always parses.
        assert_eq!(req["output_config"]["format"]["schema"]["additionalProperties"], false);
        // Thinking config is deliberately absent (model default), and no
        // sampling parameters are sent (rejected on current models).
        assert!(req.get("thinking").is_none());
        assert!(req.get("temperature").is_none());
    }

    #[test]
    fn request_uses_custom_model_when_set() {
        let req = build_request("q", "macos", "zsh", " claude-haiku-4-5 ");
        assert_eq!(req["model"], "claude-haiku-4-5");
    }

    #[test]
    fn parse_happy_path() {
        let body = serde_json::json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "{\"command\":\"find . -size +100M\",\"explanation\":\"大きいファイルを探します\",\"caution\":\"\"}" }
            ]
        })
        .to_string();
        let s = parse_response(&body).unwrap();
        assert_eq!(s.command, "find . -size +100M");
        assert!(s.caution.is_empty());
    }

    #[test]
    fn parse_refusal_and_errors() {
        let refusal = serde_json::json!({ "stop_reason": "refusal", "content": [] }).to_string();
        assert_eq!(parse_response(&refusal).unwrap_err(), "refusal");

        let api_err = serde_json::json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "invalid x-api-key" }
        })
        .to_string();
        assert!(parse_response(&api_err).unwrap_err().contains("invalid x-api-key"));

        assert!(parse_response("not json").is_err());
    }
}
