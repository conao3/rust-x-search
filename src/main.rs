use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{SecondsFormat, Utc};
use clap::{Parser, ValueEnum};
use fs2::FileExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_MODEL: &str = "grok-4.3";
const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_OAUTH_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const XAI_REFRESH_SKEW_SECONDS: u64 = 120;

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
    #[arg(long)]
    base_url: Option<String>,

    /// Authentication source. auto uses XAI_API_KEY first, then Hermes xAI OAuth.
    #[arg(long, value_enum, default_value_t = AuthMode::Auto)]
    auth: AuthMode,

    /// Environment variable that contains the xAI API key.
    #[arg(long, default_value = "XAI_API_KEY")]
    api_key_env: String,

    /// Hermes home directory that contains auth.json. Defaults to HERMES_HOME or ~/.hermes.
    #[arg(long, env = "HERMES_HOME")]
    hermes_home: Option<PathBuf>,

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

#[derive(Clone, Debug, ValueEnum)]
enum AuthMode {
    Auto,
    ApiKey,
    HermesOauth,
}

#[derive(Debug)]
struct RuntimeCredentials {
    bearer_token: String,
    base_url: String,
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

    if !args.allowed_x_handles.is_empty() && !args.excluded_x_handles.is_empty() {
        bail!("--allow-handle and --exclude-handle cannot be used together");
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout_seconds))
        .build()?;
    let credentials = resolve_runtime_credentials(&args, &client).await?;

    let body = request_body(&args)?;
    let url = format!("{}/responses", credentials.base_url.trim_end_matches('/'));

    let response = client
        .post(url)
        .bearer_auth(credentials.bearer_token)
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

async fn resolve_runtime_credentials(
    args: &Args,
    client: &reqwest::Client,
) -> Result<RuntimeCredentials> {
    match args.auth {
        AuthMode::ApiKey => resolve_api_key_credentials(args),
        AuthMode::HermesOauth => resolve_hermes_oauth_credentials(args, client).await,
        AuthMode::Auto => {
            if std::env::var_os(&args.api_key_env).is_some() {
                resolve_api_key_credentials(args)
            } else {
                resolve_hermes_oauth_credentials(args, client).await
            }
        }
    }
}

fn resolve_api_key_credentials(args: &Args) -> Result<RuntimeCredentials> {
    let bearer_token = std::env::var(&args.api_key_env)
        .with_context(|| format!("{} is not set", args.api_key_env))?;
    Ok(RuntimeCredentials {
        bearer_token,
        base_url: selected_base_url(args),
    })
}

async fn resolve_hermes_oauth_credentials(
    args: &Args,
    client: &reqwest::Client,
) -> Result<RuntimeCredentials> {
    let auth_file = auth_file_path(args)?;
    let auth_store = read_auth_store(&auth_file)?;
    let access_token = xai_access_token(&auth_store)?;

    if access_token_is_expiring(&access_token, XAI_REFRESH_SKEW_SECONDS) {
        refresh_hermes_xai_oauth_tokens(&auth_file, client).await?;
    }

    let auth_store = read_auth_store(&auth_file)?;
    Ok(RuntimeCredentials {
        bearer_token: xai_access_token(&auth_store)?,
        base_url: selected_base_url(args),
    })
}

async fn refresh_hermes_xai_oauth_tokens(auth_file: &Path, client: &reqwest::Client) -> Result<()> {
    let lock_path = auth_file.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed to lock {}", lock_path.display()))?;

    let mut auth_store = read_auth_store(auth_file)?;
    let access_token = xai_access_token(&auth_store)?;
    if !access_token_is_expiring(&access_token, XAI_REFRESH_SKEW_SECONDS) {
        return Ok(());
    }

    let refresh_token = xai_refresh_token(&auth_store)?;
    let token_endpoint = token_endpoint(&auth_store, client).await?;
    validate_xai_endpoint(&token_endpoint)?;

    let body = serde_urlencoded::to_string([
        ("grant_type", "refresh_token"),
        ("client_id", XAI_OAUTH_CLIENT_ID),
        ("refresh_token", refresh_token.as_str()),
    ])?;

    let response = client
        .post(&token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .context("failed to refresh xAI OAuth token")?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .context("failed to parse xAI token refresh response as JSON")?;
    if !status.is_success() {
        bail!("xAI OAuth token refresh failed with {status}");
    }

    let provider = xai_provider_mut(&mut auth_store)?;
    let tokens = provider
        .get_mut("tokens")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("Hermes xAI OAuth state is missing tokens"))?;
    let refreshed_access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("xAI token refresh response was missing access_token"))?;
    tokens.insert("access_token".to_string(), json!(refreshed_access_token));
    tokens.insert(
        "refresh_token".to_string(),
        json!(value
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or(&refresh_token)),
    );
    if let Some(id_token) = value.get("id_token").and_then(Value::as_str) {
        tokens.insert("id_token".to_string(), json!(id_token));
    }
    if let Some(expires_in) = value.get("expires_in") {
        tokens.insert("expires_in".to_string(), expires_in.clone());
    }
    if let Some(token_type) = value.get("token_type").and_then(Value::as_str) {
        tokens.insert("token_type".to_string(), json!(token_type));
    }

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    provider.insert("last_refresh".to_string(), json!(now.clone()));
    provider.insert("auth_mode".to_string(), json!("oauth_pkce"));
    provider
        .entry("discovery".to_string())
        .or_insert_with(|| json!({ "token_endpoint": token_endpoint }));
    auth_store["updated_at"] = json!(now);

    write_auth_store(auth_file, &auth_store)?;
    Ok(())
}

async fn token_endpoint(auth_store: &Value, client: &reqwest::Client) -> Result<String> {
    if let Some(endpoint) = auth_store
        .pointer("/providers/xai-oauth/discovery/token_endpoint")
        .and_then(Value::as_str)
        .filter(|endpoint| !endpoint.trim().is_empty())
    {
        return Ok(endpoint.trim().to_string());
    }

    let response = client
        .get(XAI_OAUTH_DISCOVERY_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .context("xAI OIDC discovery failed")?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .context("failed to parse xAI OIDC discovery response as JSON")?;
    if !status.is_success() {
        bail!("xAI OIDC discovery returned {status}");
    }
    let endpoint = value
        .get("token_endpoint")
        .and_then(Value::as_str)
        .filter(|endpoint| !endpoint.trim().is_empty())
        .ok_or_else(|| anyhow!("xAI OIDC discovery response was missing token_endpoint"))?;
    Ok(endpoint.trim().to_string())
}

fn selected_base_url(args: &Args) -> String {
    args.base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .or_else(|| {
            std::env::var("HERMES_XAI_BASE_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            std::env::var("XAI_BASE_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

fn auth_file_path(args: &Args) -> Result<PathBuf> {
    let hermes_home = args
        .hermes_home
        .clone()
        .or_else(|| std::env::var_os("HERMES_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".hermes")))
        .ok_or_else(|| {
            anyhow!("could not resolve Hermes home; set --hermes-home or HERMES_HOME")
        })?;
    Ok(hermes_home.join("auth.json"))
}

fn read_auth_store(auth_file: &Path) -> Result<Value> {
    let text = fs::read_to_string(auth_file).with_context(|| {
        format!(
            "failed to read Hermes auth store at {}",
            auth_file.display()
        )
    })?;
    serde_json::from_str(&text).with_context(|| {
        format!(
            "failed to parse Hermes auth store at {}",
            auth_file.display()
        )
    })
}

fn write_auth_store(auth_file: &Path, auth_store: &Value) -> Result<()> {
    let payload = format!("{}\n", serde_json::to_string_pretty(auth_store)?);
    let tmp_path = auth_file.with_file_name(format!(
        "{}.tmp.{}",
        auth_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("auth.json"),
        std::process::id()
    ));
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, auth_file).with_context(|| {
        format!(
            "failed to replace {} with {}",
            auth_file.display(),
            tmp_path.display()
        )
    })?;
    Ok(())
}

fn xai_provider_mut(auth_store: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    auth_store
        .pointer_mut("/providers/xai-oauth")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("No Hermes xAI OAuth credentials stored. Run `hermes model` and select xAI Grok OAuth."))
}

fn xai_access_token(auth_store: &Value) -> Result<String> {
    auth_store
        .pointer("/providers/xai-oauth/tokens/access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Hermes xAI OAuth state is missing access_token. Run `hermes model` to re-authenticate."))
}

fn xai_refresh_token(auth_store: &Value) -> Result<String> {
    auth_store
        .pointer("/providers/xai-oauth/tokens/refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Hermes xAI OAuth state is missing refresh_token. Run `hermes model` to re-authenticate."))
}

fn access_token_is_expiring(access_token: &str, skew_seconds: u64) -> bool {
    let Some(payload) = access_token.split('.').nth(1) else {
        return false;
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(exp) = value.get("exp").and_then(Value::as_u64) else {
        return false;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };
    exp <= now.as_secs().saturating_add(skew_seconds)
}

fn validate_xai_endpoint(endpoint: &str) -> Result<()> {
    let url = Url::parse(endpoint).with_context(|| format!("invalid xAI endpoint: {endpoint}"))?;
    if url.scheme() != "https" {
        bail!("xAI OAuth token endpoint must use https");
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "x.ai" && !host.ends_with(".x.ai") {
        bail!("xAI OAuth token endpoint host must be x.ai or a *.x.ai subdomain");
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

    #[test]
    fn validates_xai_endpoint_hosts() {
        assert!(validate_xai_endpoint("https://auth.x.ai/oauth2/token").is_ok());
        assert!(validate_xai_endpoint("https://evil.example/token").is_err());
        assert!(validate_xai_endpoint("http://auth.x.ai/token").is_err());
    }

    #[test]
    fn treats_non_jwt_tokens_as_not_expiring() {
        assert!(!access_token_is_expiring(
            "opaque-token",
            XAI_REFRESH_SKEW_SECONDS
        ));
    }
}
