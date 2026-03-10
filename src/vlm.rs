use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default prompt for describe mode.
pub const DEFAULT_DESCRIBE_PROMPT: &str =
    "Describe this photograph in 1-3 concise sentences. Focus on the subject, setting, lighting, and mood. Be specific about what you see, not what you interpret.";

/// Result of a single VLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescribeResult {
    pub asset_id: String,
    pub description: Option<String>,
    pub status: DescribeStatus,
}

/// Status of a single describe operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescribeStatus {
    Described,
    Skipped(String),
    Error(String),
}

/// Aggregate result for batch describe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchDescribeResult {
    pub described: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub dry_run: bool,
    pub results: Vec<DescribeResult>,
}

/// Call a VLM endpoint with an image.
///
/// Tries OpenAI-compatible `/v1/chat/completions` first, falls back to
/// Ollama's native `/api/generate` on 404.
pub fn call_vlm(
    endpoint: &str,
    model: &str,
    image_base64: &str,
    prompt: &str,
    max_tokens: u32,
    timeout: u32,
    debug: bool,
) -> Result<String> {
    // Try OpenAI-compatible endpoint first
    match call_openai_compatible(endpoint, model, image_base64, prompt, max_tokens, timeout, debug)
    {
        Ok(text) => return Ok(text),
        Err(e) => {
            let err_str = format!("{e}");
            if err_str.contains("404") || err_str.contains("not found") {
                if debug {
                    eprintln!("  [debug] /v1/chat/completions returned 404, falling back to /api/generate");
                }
                // Fall back to Ollama native API
                return call_ollama_native(
                    endpoint,
                    model,
                    image_base64,
                    prompt,
                    max_tokens,
                    timeout,
                    debug,
                );
            }
            return Err(e);
        }
    }
}

/// Call the OpenAI-compatible /v1/chat/completions endpoint.
fn call_openai_compatible(
    endpoint: &str,
    model: &str,
    image_base64: &str,
    prompt: &str,
    max_tokens: u32,
    timeout: u32,
    debug: bool,
) -> Result<String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/jpeg;base64,{image_base64}")
                    }
                },
                {
                    "type": "text",
                    "text": prompt
                }
            ]
        }],
        "max_tokens": max_tokens,
        "temperature": 0.3,
        "stream": false
    });

    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));
    let response = curl_post(&url, &body, timeout, debug)?;

    // Parse OpenAI response format
    let resp: serde_json::Value =
        serde_json::from_str(&response).context("Failed to parse VLM response as JSON")?;

    // Check for error response
    if let Some(err) = resp.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("VLM error: {msg}");
    }

    let text = resp
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Unexpected VLM response format: no choices[0].message.content"))?;

    Ok(text.trim().to_string())
}

/// Call Ollama's native /api/generate endpoint.
fn call_ollama_native(
    endpoint: &str,
    model: &str,
    image_base64: &str,
    prompt: &str,
    _max_tokens: u32,
    timeout: u32,
    debug: bool,
) -> Result<String> {
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "images": [image_base64],
        "stream": false
    });

    let url = format!("{}/api/generate", endpoint.trim_end_matches('/'));
    let response = curl_post(&url, &body, timeout, debug)?;

    let resp: serde_json::Value =
        serde_json::from_str(&response).context("Failed to parse Ollama response as JSON")?;

    if let Some(err) = resp.get("error") {
        let msg = err.as_str().unwrap_or("unknown error");
        anyhow::bail!("Ollama error: {msg}");
    }

    let text = resp
        .get("response")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Unexpected Ollama response format: no 'response' field"))?;

    Ok(text.trim().to_string())
}

/// Send a POST request via curl with JSON body on stdin.
fn curl_post(
    url: &str,
    body: &serde_json::Value,
    timeout: u32,
    debug: bool,
) -> Result<String> {
    let body_str = serde_json::to_string(body)?;

    if debug {
        eprintln!("  [debug] POST {url} (body: {} bytes)", body_str.len());
    }

    let mut child = Command::new("curl")
        .args([
            "-sS",
            "-X",
            "POST",
            url,
            "-H",
            "Content-Type: application/json",
            "-d",
            "@-",
            "--max-time",
            &timeout.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to run curl. Is curl installed?")?;

    // Write body to stdin
    if let Some(ref mut stdin) = child.stdin {
        stdin
            .write_all(body_str.as_bytes())
            .context("Failed to write to curl stdin")?;
    }
    // Drop stdin to signal EOF
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .context("Failed to wait for curl")?;

    if debug {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            eprintln!("  [debug] curl stderr: {stderr}");
        }
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Detect connection refused (common: Ollama not running)
        if stderr.contains("Connection refused") || stderr.contains("couldn't connect") {
            anyhow::bail!(
                "VLM server not reachable at {url}. Start Ollama with `ollama serve` or check your endpoint configuration."
            );
        }
        // Detect timeout
        if stderr.contains("timed out") || stderr.contains("Operation timeout") {
            anyhow::bail!("VLM request timed out after {timeout}s");
        }
        anyhow::bail!("curl failed (exit {}): {}{}", output.status, stderr, stdout);
    }

    let response = String::from_utf8(output.stdout)
        .context("VLM response is not valid UTF-8")?;

    // Detect HTTP error status from curl output
    if response.starts_with("<!DOCTYPE") || response.starts_with("<html") {
        anyhow::bail!("VLM endpoint returned HTML (404 or error page)");
    }

    Ok(response)
}

/// Check if a VLM endpoint is reachable.
pub fn check_endpoint(endpoint: &str, timeout: u32, debug: bool) -> Result<String> {
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));

    if debug {
        eprintln!("  [debug] GET {url}");
    }

    let output = Command::new("curl")
        .args(["-sS", "--max-time", &timeout.to_string(), &url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to run curl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Connection refused") || stderr.contains("couldn't connect") {
            anyhow::bail!(
                "VLM server not reachable at {}. Start Ollama with `ollama serve`.",
                endpoint
            );
        }
        anyhow::bail!("curl failed: {}", stderr);
    }

    let response = String::from_utf8_lossy(&output.stdout);

    // Try to parse as Ollama /api/tags response
    if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&response) {
        if let Some(models) = resp.get("models").and_then(|m| m.as_array()) {
            let names: Vec<&str> = models
                .iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .collect();
            return Ok(format!(
                "Connected to {}. {} model(s) available: {}",
                endpoint,
                names.len(),
                names.join(", ")
            ));
        }
    }

    Ok(format!("Connected to {endpoint}. Server is responding."))
}

/// Read an image file and return its base64 encoding.
pub fn encode_image_base64(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open image: {}", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    // Use a simple base64 encoder (no extra dependency)
    Ok(base64_encode(&buf))
}

/// Simple base64 encoder (avoids adding a base64 crate dependency).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_base64_encode_hello() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
    }

    #[test]
    fn test_base64_encode_hello_world() {
        assert_eq!(base64_encode(b"Hello, World!"), "SGVsbG8sIFdvcmxkIQ==");
    }

    #[test]
    fn test_base64_encode_three_bytes() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn test_base64_encode_one_byte() {
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn test_base64_encode_two_bytes() {
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn test_default_describe_prompt() {
        assert!(DEFAULT_DESCRIBE_PROMPT.contains("photograph"));
    }
}
