//! AI provider client (OpenAI-compatible) with thinking-level control.

use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::store::Provider;

/// Preset instructions. These are the *initial* content of each action's prompt field —
/// the user edits them like any other prompt, and `store` migrates old installs to this
/// shape so the action editor needs no type selector.
pub const PRESET_TRANSLATE: &str = "你是一个专业翻译引擎。将用户提供的文本翻译出来：原文是中文则译成英文，其他语言则译成简体中文。只输出译文本身，不要添加解释、引号或前后缀。";
pub const PRESET_SUMMARIZE: &str =
    "你是一个内容摘要助手。用简体中文简要总结用户提供的文本的核心要点，使用简短的要点列表（最多 5 条）。只输出总结内容本身。";
pub const PRESET_EXPLAIN: &str =
    "你是一个知识讲解助手。用简体中文通俗易懂地解释用户提供的文本（术语、代码或概念），必要时给出例子。只输出解释内容本身。";
pub const PRESET_REWRITE: &str =
    "你是一个文字润色助手。在不改变原意的前提下润色用户提供的文本，使其更通顺、专业、简洁。只输出改写后的文本本身。";

/// Instructions for the legacy built-in ids, used by the store migration.
pub fn builtin_prompt(builtin: &str) -> &'static str {
    match builtin {
        "translate" => PRESET_TRANSLATE,
        "summarize" => PRESET_SUMMARIZE,
        "explain" => PRESET_EXPLAIN,
        "rewrite" => PRESET_REWRITE,
        _ => "",
    }
}

/// The system prompt for an action: its own prompt text, with a neutral fallback when the
/// user left it empty.
pub fn system_prompt(prompt: &str) -> String {
    let p = prompt.trim();
    if p.is_empty() {
        "你是文本处理助手。按用户的要求处理文本，只输出处理结果。".to_string()
    } else {
        p.to_string()
    }
}

fn endpoint(api_base: &str) -> String {
    let base = api_base.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

/// Resolve the effective level: per-action override > provider level > global.
pub fn resolve_level(provider: &Provider, action_level: Option<&str>, global_level: &str) -> String {
    if let Some(l) = action_level {
        if l != "inherit" && !l.is_empty() {
            return l.to_string();
        }
    }
    if let Some(l) = provider.thinking_level.as_deref() {
        if !l.is_empty() && l != "auto" {
            return l.to_string();
        }
    }
    global_level.to_string()
}

pub fn supports_thinking(provider: &Provider) -> bool {
    match provider.thinking.as_str() {
        "off" => false,
        "on" => true,
        _ => {
            let m = provider.model.to_lowercase();
            [
                "deepseek", "reasoner", "thinking", "qwen", "qwq", "glm", "kimi", "moonshot", "claude",
                "gemini", "magistral", "gpt-oss", "o1", "o3", "o4",
            ]
            .iter()
            .any(|k| m.contains(k))
        }
    }
}

/// Build the request headers for a provider.
///
/// Beyond the bearer token, gateways may require identification headers:
///   - OpenCode Go/Zen rejects requests without `x-opencode-session` (HTTP 400
///     `MissingSessionID`); Cherry Studio fills it from the conversation id, and a stable
///     per-install value serves the same purpose here.
///   - A descriptive User-Agent is requested by OpenCode and is good practice generally.
/// Any user-supplied headers win over the defaults.
fn apply_headers(mut req: reqwest::RequestBuilder, provider: &Provider) -> reqwest::RequestBuilder {
    req = req.header("User-Agent", concat!("Wisp/", env!("CARGO_PKG_VERSION")));
    if !provider.api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", provider.api_key.trim()));
    }

    let mut has_session = false;
    for h in &provider.headers {
        if h.name.trim().is_empty() {
            continue;
        }
        if h.name.eq_ignore_ascii_case("x-opencode-session") {
            has_session = true;
        }
        req = req.header(h.name.trim(), h.value.clone());
    }

    // Auto-fill the OpenCode session header when the endpoint is OpenCode and the user has
    // not supplied their own.
    if !has_session && provider.api_base.to_lowercase().contains("opencode.ai") {
        let sid = crate::store::load().session_id;
        req = req.header("x-opencode-session", sid);
    }
    req
}

/// One message in a conversation.
///
/// Follow-up questions need the whole history, so requests are built from a message list
/// rather than a fixed (system, user) pair. The definition lives in `chat.rs` so the panel
/// state machine can be tested without this module's HTTP client.
pub use crate::chat::ChatMessage;

/// Build the request body, adding provider-specific thinking parameters only when
/// the level is meaningful (so plain "auto" stays a vanilla OpenAI call).
pub fn build_body(provider: &Provider, messages: &[ChatMessage], level: &str, stream: bool) -> Value {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();

    let mut body = json!({
        "model": provider.model,
        "messages": msgs,
        "stream": stream
    });
    if let Some(t) = provider.temperature {
        if !t.is_nan() {
            body["temperature"] = json!(t);
        }
    }
    if let Some(m) = provider.max_tokens {
        if m > 0 {
            body["max_tokens"] = json!(m);
        }
    }

    if level.is_empty() || level == "auto" {
        return body;
    }
    let model = provider.model.to_lowercase();
    let disabled = level == "off";

    if model.contains("claude") || model.contains("anthropic") {
        body["thinking"] = if disabled {
            json!({ "type": "disabled" })
        } else {
            json!({ "type": "enabled", "budget_tokens": if level == "high" { 4096 } else if level == "low" { 1024 } else { 2048 } })
        };
    } else if model.contains("gemini") {
        body["extra_body"] = json!({
            "google": { "thinking_config": { "thinking_budget": if disabled { 0 } else if level == "high" { 8192 } else { 2048 } } }
        });
    } else {
        // `reasoning_effort` only accepts none/low/medium/high; send `none` to disable and
        // nothing else when the gateway might reject unknown levels.
        body["reasoning_effort"] = json!(if disabled { "none" } else { level });
        if model.contains("qwen") || model.contains("qwq") {
            body["enable_thinking"] = json!(!disabled);
        }
        if model.contains("deepseek") {
            body["thinking"] = json!(if disabled { json!({"type":"disabled"}) } else { json!({"type":"enabled"}) });
        }
    }
    body
}

pub struct StreamResult {
    pub text: String,
    pub reasoning: String,
}

fn extract_delta(v: &Value) -> String {
    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
        return String::new();
    };
    if let Some(s) = choice.get("delta").and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = choice.get("delta").and_then(|d| d.get("content")).and_then(|c| c.as_array()) {
        return arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    if let Some(s) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
        return s.to_string();
    }
    String::new()
}

fn extract_reasoning(v: &Value) -> String {
    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
        return String::new();
    };
    let d = choice.get("delta").or_else(|| choice.get("message"));
    let Some(d) = d else { return String::new() };
    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(s) = d.get(key).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    if let Some(arr) = d.get("content").and_then(|c| c.as_array()) {
        return arr
            .iter()
            .filter(|p| {
                matches!(p.get("type").and_then(|t| t.as_str()), Some("thinking") | Some("reasoning"))
            })
            .filter_map(|p| p.get("thinking").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

/// Incremental SSE line splitter and small text helpers.
///
/// They live in `sse.rs` so an integration test can exercise them without the Tauri runtime
/// (see `tests/sse.rs`).
pub use crate::sse::{truncate_chars, SseBuffer};

/// Parse one SSE `data:` payload, returning (answer text, reasoning text).
fn parse_sse_data(data: &str) -> Result<(String, String), String> {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok((String::new(), String::new()));
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return Ok((String::new(), String::new()));
    };
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| err.as_str())
            .unwrap_or("模型返回错误");
        return Err(msg.to_string());
    }
    Ok((extract_delta(&v), extract_reasoning(&v)))
}

/// Stream a completion. `on_chunk` receives answer text, `on_reasoning` thinking text.
pub async fn chat_stream<F, R>(
    provider: &Provider,
    messages: &[ChatMessage],
    level: &str,
    mut on_chunk: F,
    mut on_reasoning: R,
) -> Result<StreamResult, String>
where
    F: FnMut(&str),
    R: FnMut(&str),
{
    if provider.model.is_empty() {
        return Err("未配置模型".into());
    }
    if provider.api_base.trim().is_empty() {
        return Err("API 地址为空".into());
    }
    let url = endpoint(&provider.api_base);
    let body = build_body(provider, messages, level, true);

    let client = reqwest::Client::new();
    let req = apply_headers(client.post(&url).json(&body), provider);
    let res = req.send().await.map_err(|e| format!("请求失败: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let detail = res.text().await.unwrap_or_default();
        let detail = truncate_chars(&detail, 400);
        return Err(format!("HTTP {status}: {detail}"));
    }

    let mut stream = res.bytes_stream();
    let mut sse = SseBuffer::default();
    let mut out = StreamResult {
        text: String::new(),
        reasoning: String::new(),
    };

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("流读取失败: {e}"))?;
        for line in sse.push(&bytes) {
            let Some(data) = line.strip_prefix("data:") else { continue };
            let (delta, reasoning) = parse_sse_data(data)?;
            if !reasoning.is_empty() {
                out.reasoning.push_str(&reasoning);
                on_reasoning(&reasoning);
            }
            if !delta.is_empty() {
                out.text.push_str(&delta);
                on_chunk(&delta);
            }
        }
    }
    if let Some(line) = sse.finish() {
        if let Some(data) = line.strip_prefix("data:") {
            let (delta, reasoning) = parse_sse_data(data)?;
            if !reasoning.is_empty() {
                out.reasoning.push_str(&reasoning);
                on_reasoning(&reasoning);
            }
            if !delta.is_empty() {
                out.text.push_str(&delta);
                on_chunk(&delta);
            }
        }
    }
    Ok(out)
}

/// One tiny non-streaming request to validate connectivity.
pub async fn test_provider(provider: &Provider) -> (bool, String) {
    let url = endpoint(&provider.api_base);
    let body = build_body(
        provider,
        &[ChatMessage::system("Reply with the single word: ok"), ChatMessage::user("ping")],
        "off",
        false,
    );
    let client = reqwest::Client::new();
    let req = apply_headers(client.post(&url).json(&body), provider);
    match req.send().await {
        Ok(res) => {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            if !status.is_success() {
                return (false, format!("HTTP {status}: {}", truncate_chars(&text, 300)));
            }
            let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
            let msg = extract_delta(&v);
            let msg = if msg.is_empty() { "(空响应，但连接成功)".to_string() } else { msg };
            (true, truncate_chars(&msg, 60))
        }
        Err(e) => (false, format!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_error_payload_surfaces_message() {
        let e = parse_sse_data("{\"error\":{\"message\":\"boom\"}}").unwrap_err();
        assert_eq!(e, "boom");
    }
}
