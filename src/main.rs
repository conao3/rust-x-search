use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_MODEL: &str = "grok-4.3";

#[derive(Debug, Parser)]
#[command(name = "x-search")]
#[command(about = "Search X through the xAI Responses API x_search tool")]
struct Args {
    /// Query or question to answer using X Search.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
    query: Vec<String>,

    /// xAI model to call.
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,

    /// xAI API base URL.
    #[arg(long, default_value = DEFAULT_BASE_URL)]
    base_url: String,

    /// Environment variable that contains the xAI API key.
    #[arg(long, default_value = "XAI_API_KEY")]
    api_key_env: String,

    /// Restrict X Search to these handles. Repeat or comma-separate values.
    #[arg(long = "allow-handle", short = 'a', value_delimiter = ',')]
    allowed_x_handles: Vec<String>,

    /// Exclude these handles from X Search. Repeat or comma-separate values.
    #[arg(long = "exclude-handle", short = 'x', value_delimiter = ',')]
    excluded_x_handles: Vec<String>,

    /// Start date for search range, in YYYY-MM-DD format.
    #[arg(long)]
    from_date: Option<String>,

    /// End date for search range, in YYYY-MM-DD format.
    #[arg(long)]
    to_date: Option<String>,

    /// Ask X Search to analyze images in posts.
    #[arg(long)]
    image_understanding: bool,

    /// Ask X Search to analyze videos in posts.
    #[arg(long)]
    video_understanding: bool,

    /// System prompt sent before the user query.
    #[arg(
        long,
        default_value = "Use X Search when useful. Answer directly, cite relevant X posts, and do not invent posts or metrics."
    )]
    system: String,

    /// HTTP timeout in seconds.
    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,

    /// Print the full API response JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct ExtractedResponse {
    text: String,
    citations: Vec<Value>,
    raw: Value,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let api_key = std::env::var(&args.api_key_env)
        .with_context(|| format!("{} is not set", args.api_key_env))?;

    if !args.allowed_x_handles.is_empty() && !args.excluded_x_handles.is_empty() {
        bail!("--allow-handle and --exclude-handle cannot be used together");
    }

    let body = request_body(&args)?;
    let url = format!("{}/responses", args.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout_seconds))
        .build()?;

    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .context("failed to send request to xAI")?;

    let status = response.status();
    let value: Value = response
        .json()
        .await
        .context("failed to parse xAI response as JSON")?;

    if !status.is_success() {
        bail!("xAI API returned {status}: {}", pretty_json(&value));
    }

    let extracted = extract_response(value);
    if args.json {
        println!("{}", pretty_json(&extracted.raw));
    } else {
        print_text_response(&extracted)?;
    }

    Ok(())
}

fn request_body(args: &Args) -> Result<Value> {
    let query = args.query.join(" ");
    let mut tool = json!({ "type": "x_search" });
    let tool_obj = tool
        .as_object_mut()
        .ok_or_else(|| anyhow!("internal error: tool is not an object"))?;

    if !args.allowed_x_handles.is_empty() {
        tool_obj.insert(
            "allowed_x_handles".to_string(),
            json!(normalize_handles(&args.allowed_x_handles)),
        );
    }
    if !args.excluded_x_handles.is_empty() {
        tool_obj.insert(
            "excluded_x_handles".to_string(),
            json!(normalize_handles(&args.excluded_x_handles)),
        );
    }
    if let Some(from_date) = &args.from_date {
        tool_obj.insert("from_date".to_string(), json!(from_date));
    }
    if let Some(to_date) = &args.to_date {
        tool_obj.insert("to_date".to_string(), json!(to_date));
    }
    if args.image_understanding {
        tool_obj.insert("enable_image_understanding".to_string(), json!(true));
    }
    if args.video_understanding {
        tool_obj.insert("enable_video_understanding".to_string(), json!(true));
    }

    Ok(json!({
        "model": args.model,
        "input": [
            { "role": "system", "content": args.system },
            { "role": "user", "content": query }
        ],
        "tools": [tool]
    }))
}

fn normalize_handles(handles: &[String]) -> Vec<String> {
    handles
        .iter()
        .map(|handle| handle.trim().trim_start_matches('@').to_string())
        .filter(|handle| !handle.is_empty())
        .collect()
}

fn extract_response(raw: Value) -> ExtractedResponse {
    let text = raw
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| extract_text_from_output(raw.get("output")))
        .unwrap_or_default();

    let citations = raw
        .get("citations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    ExtractedResponse {
        text,
        citations,
        raw,
    }
}

fn extract_text_from_output(output: Option<&Value>) -> Option<String> {
    let mut parts = Vec::new();
    let output = output?.as_array()?;

    for item in output {
        if let Some(content) = item.get("content").and_then(Value::as_array) {
            for chunk in content {
                if let Some(text) = chunk.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                } else if let Some(text) = chunk.get("output_text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn print_text_response(response: &ExtractedResponse) -> Result<()> {
    if response.text.trim().is_empty() {
        bail!(
            "response did not contain output text; rerun with --json to inspect the raw response"
        );
    }

    println!("{}", response.text.trim());

    if !response.citations.is_empty() {
        println!("\nCitations:");
        for citation in &response.citations {
            if let Some(url) = citation.get("url").and_then(Value::as_str) {
                println!("- {url}");
            } else {
                println!("- {}", citation);
            }
        }
    }

    Ok(())
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_handles() {
        let handles = vec!["@xai".to_string(), " elonmusk ".to_string(), "".to_string()];
        assert_eq!(normalize_handles(&handles), vec!["xai", "elonmusk"]);
    }

    #[test]
    fn extracts_output_text() {
        let value = json!({ "output_text": "hello" });
        assert_eq!(extract_response(value).text, "hello");
    }

    #[test]
    fn extracts_nested_output_text() {
        let value = json!({
            "output": [
                { "type": "message", "content": [
                    { "type": "output_text", "text": "hello" },
                    { "type": "output_text", "text": "world" }
                ]}
            ]
        });
        assert_eq!(extract_response(value).text, "hello\nworld");
    }
}
