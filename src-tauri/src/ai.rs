//! AI provider client (OpenAI-compatible) with thinking-level control.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::store::Provider;

pub fn builtin_prompt(builtin: &str) -> &'static str {
    match builtin {
        "translate" => "你是一个专业翻译引擎。将用户提供的文本翻译出来：原文是中文则译成英文，其他语言则译成简体中文。只输出译文本身，不要添加解释、引号或前后缀。",
        "summarize" => "你是一个内容摘要助手。用简体中文简要总结用户提供的文本的核心要点，使用简短的要点列表（最多 5 条）。只输出总结内容本身。",
        "explain" => "你是一个知识讲解助手。用简体中文通俗易懂地解释用户提供的文本（术语、代码或概念），必要时给出例子。只输出解释内容本身。",
        "rewrite" => "你是一个文字润色助手。在不改变原意的前提下润色用户提供的文本，使其更通顺、专业、简洁。只输出改写后的文本本身。",
        _ => "你是一个乐于助人的文本处理助手。按照用户的要求处理文本，只输出处理结果，不要添加额外说明。",
    }
}

pub fn system_prompt(builtin: &str, custom: &str) -> String {
    let base = builtin_prompt(builtin);
    if custom.trim().is_empty() {
        base.to_string()
    } else {
        format!("{base}\n\n附加要求：{custom}")
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
                "deepseek", "reasoner", "thinking", "qwen", "qwq", "glm", "kimi", "moonshot", "claude", "gemini",
                "magistral", "gpt-oss", "o1", "o3", "o4",
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
    req = req.header("User-Agent", "Wisp/1.0 (+https://github.com/)");
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
/// rather than a fixed (system, user) pair.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

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
            .filter(|p| matches!(p.get("type").and_then(|t| t.as_str()), Some("thinking") | Some("reasoning")))
            .filter_map(|p| p.get("thinking").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
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
    let url = endpoint(&provider.api_base);
    if provider.api_base.trim().is_empty() {
        return Err("API 地址为空".into());
    }
    let body = build_body(provider, messages, level, true);

    let client = reqwest::Client::new();
    let req = apply_headers(client.post(&url).json(&body), provider);
    let res = req.send().await.map_err(|e| format!("请求失败: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let detail = res.text().await.unwrap_or_default();
        let detail = &detail[..detail.len().min(400)];
        return Err(format!("HTTP {status}: {detail}"));
    }

    let mut stream = res.bytes_stream();
    let mut buf = String::new();
    let mut out = StreamResult {
        text: String::new(),
        reasoning: String::new(),
    };

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("流读取失败: {e}"))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf.drain(..=pos);
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(err) = v.get("error") {
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("模型返回错误");
                return Err(msg.to_string());
            }
            let r = extract_reasoning(&v);
            if !r.is_empty() {
                out.reasoning.push_str(&r);
                on_reasoning(&r);
            }
            let d = extract_delta(&v);
            if !d.is_empty() {
                out.text.push_str(&d);
                on_chunk(&d);
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
                let d = &text[..text.len().min(300)];
                return (false, format!("HTTP {status}: {d}"));
            }
            let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
            let msg = extract_delta(&v);
            (true, if msg.is_empty() { "(空响应，但连接成功)".into() } else { msg.chars().take(60).collect() })
        }
        Err(e) => (false, format!("{e}")),
    }
}
