mod error;
mod grammar;
mod output;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{SecondsFormat, Utc};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use output::{OutputFormat, Response};
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_MODEL: &str = "grok-4.3";
const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_OAUTH_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const XAI_REFRESH_SKEW_SECONDS: u64 = 120;
const XAI_OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_OAUTH_REDIRECT_HOST: &str = "127.0.0.1";
const XAI_OAUTH_REDIRECT_PORT: u16 = 56121;
const XAI_OAUTH_REDIRECT_PATH: &str = "/callback";
const DEFAULT_CONFIG_SUBDIR: &str = "x-search";
const AUTH_FILE_NAME: &str = "auth.json";
const AUTH_STORE_VERSION: u64 = 1;

const DEFAULT_SYSTEM_PROMPT: &str = "Use X Search when useful. Answer directly, cite relevant X posts, and do not invent posts or metrics.";

#[derive(Parser)]
#[command(
    name = "x-search",
    version,
    about = "Search X through the xAI Responses API x_search tool"
)]
struct Cli {
    /// Output format
    #[arg(long, global = true, help_heading = "Global Options", value_enum, default_value_t = OutputFormat::Json)]
    output: OutputFormat,
    /// Print debug logs to stderr
    #[arg(long, global = true, help_heading = "Global Options")]
    debug: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Search X posts and answer a question with citations
    #[command(subcommand)]
    Post(PostCommand),
    /// Manage the xAI OAuth credentials stored in the x-search auth store
    #[command(subcommand)]
    Auth(AuthCommand),
}

#[derive(Subcommand)]
enum PostCommand {
    /// Answer a query with the xAI Responses API x_search tool
    #[command(name = "search-posts")]
    SearchPosts(Box<SearchPostsArgs>),
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign in to xAI Grok OAuth (loopback callback) and store the credentials. Progress goes to stderr
    #[command(name = "start-oauth-login")]
    StartOauthLogin(StartOauthLoginArgs),
    /// Show whether xAI OAuth credentials are stored, without printing tokens
    #[command(name = "get-oauth-status")]
    GetOauthStatus(GetOauthStatusArgs),
}

#[derive(Args)]
struct ConfigArgs {
    /// Directory that stores auth.json. Defaults to X_SEARCH_CONFIG_DIR, XDG_CONFIG_HOME/x-search, or ~/.config/x-search
    #[arg(long, env = "X_SEARCH_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
}

#[derive(Args)]
struct SearchPostsArgs {
    /// Query or question to answer using X Search
    #[arg(long, allow_hyphen_values = true)]
    query: String,
    /// xAI model to call
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// xAI API base URL (defaults to XAI_BASE_URL or https://api.x.ai/v1)
    #[arg(long)]
    base_url: Option<String>,
    /// Authentication source. auto uses the API key environment variable first, then xAI OAuth
    #[arg(long, value_enum, default_value_t = AuthMode::Auto)]
    auth: AuthMode,
    /// Environment variable that contains the xAI API key
    #[arg(long, default_value = "XAI_API_KEY")]
    api_key_env: String,
    #[command(flatten)]
    config: ConfigArgs,
    /// Restrict X Search to these handles (space-separated)
    #[arg(long, num_args = 1.., conflicts_with = "excluded_handles")]
    allowed_handles: Vec<String>,
    /// Exclude these handles from X Search (space-separated)
    #[arg(long, num_args = 1..)]
    excluded_handles: Vec<String>,
    /// Start date of the search range (YYYY-MM-DD)
    #[arg(long)]
    from_date: Option<String>,
    /// End date of the search range (YYYY-MM-DD)
    #[arg(long)]
    to_date: Option<String>,
    /// Ask X Search to analyze images in posts
    #[arg(long, overrides_with = "no_image_understanding")]
    image_understanding: bool,
    /// Turn off --image-understanding
    #[arg(long, overrides_with = "image_understanding")]
    no_image_understanding: bool,
    /// Ask X Search to analyze videos in posts
    #[arg(long, overrides_with = "no_video_understanding")]
    video_understanding: bool,
    /// Turn off --video-understanding
    #[arg(long, overrides_with = "video_understanding")]
    no_video_understanding: bool,
    /// System prompt sent before the user query
    #[arg(long, allow_hyphen_values = true, default_value = DEFAULT_SYSTEM_PROMPT)]
    system_prompt: String,
    /// HTTP timeout in seconds
    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
    /// Add the full API response (keys converted to PascalCase) as Response
    #[arg(long, overrides_with = "no_include_response")]
    include_response: bool,
    /// Turn off --include-response
    #[arg(long, overrides_with = "include_response")]
    no_include_response: bool,
}

#[derive(Args)]
struct StartOauthLoginArgs {
    #[command(flatten)]
    config: ConfigArgs,
    /// Re-run the OAuth flow even when non-expiring credentials already exist
    #[arg(long, overrides_with = "no_force")]
    force: bool,
    /// Turn off --force
    #[arg(long, overrides_with = "force")]
    no_force: bool,
    /// Open the authorization URL in a browser (on by default)
    #[arg(long, default_value_t = true)]
    open_browser: bool,
    /// Only print the authorization URL to stderr
    #[arg(long)]
    no_open_browser: bool,
    /// OAuth HTTP timeout in seconds. The callback wait is nine times this, at least 180 seconds
    #[arg(long, default_value_t = 20)]
    timeout_seconds: u64,
}

#[derive(Args)]
struct GetOauthStatusArgs {
    #[command(flatten)]
    config: ConfigArgs,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthMode {
    Auto,
    ApiKey,
    Oauth,
}

#[derive(Debug)]
struct RuntimeCredentials {
    bearer_token: String,
    base_url: String,
}

#[derive(Debug)]
struct ServiceError(String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

fn service(message: String) -> anyhow::Error {
    anyhow::Error::new(ServiceError(message))
}

macro_rules! service_bail {
    ($($arg:tt)*) => {
        return Err(service(format!($($arg)*)))
    };
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SearchPostsOutput {
    text: String,
    citations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<Value>,
}

#[derive(Serialize)]
enum LoginResult {
    LoggedIn,
    AlreadyLoggedIn,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StartOauthLoginOutput {
    auth_file: String,
    result: LoginResult,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct GetOauthStatusOutput {
    auth_file: String,
    oauth_configured: bool,
    access_token_expiring: bool,
}

fn classify(error: anyhow::Error) -> error::Error {
    let message = format!("{error:#}");
    if error.chain().any(|cause| cause.is::<ServiceError>()) {
        error::target(message)
    } else {
        error::other(message)
    }
}

fn to_value<T: Serialize>(value: &T) -> error::Result<Value> {
    serde_json::to_value(value).map_err(|error| error::other(error.to_string()))
}

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            error.exit_code()
        }
    };
    std::process::exit(code);
}

async fn run() -> error::Result<()> {
    let mut root = Cli::command();
    root.build();
    let argv = grammar::prepare(&root, std::env::args_os().collect())?;
    let debug = argv.iter().any(|token| token == "--debug");
    if debug {
        eprintln!("debug: argv {:?}", &argv[1..]);
    }

    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 252 } else { 0 };
            let _ = error.print();
            std::process::exit(code);
        }
    };

    let (name, response) = match cli.command {
        Command::Post(PostCommand::SearchPosts(args)) => {
            if args.query.trim().is_empty() {
                return Err(error::parameter("--query must not be empty"));
            }
            let output = search_posts(&args, debug).await.map_err(classify)?;
            let text = text_response(&output);
            (
                "search-posts",
                Response {
                    value: to_value(&output)?,
                    text: Some(text),
                },
            )
        }
        Command::Auth(AuthCommand::StartOauthLogin(args)) => {
            let output = start_oauth_login(&args).await.map_err(classify)?;
            (
                "start-oauth-login",
                Response {
                    value: to_value(&output)?,
                    text: None,
                },
            )
        }
        Command::Auth(AuthCommand::GetOauthStatus(args)) => {
            let output = get_oauth_status(&args.config).map_err(classify)?;
            (
                "get-oauth-status",
                Response {
                    value: to_value(&output)?,
                    text: None,
                },
            )
        }
    };
    print!(
        "{}",
        output::render(&grammar::pascal_case(name), response, cli.output)?
    );
    Ok(())
}

async fn search_posts(args: &SearchPostsArgs, debug: bool) -> Result<SearchPostsOutput> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout_seconds))
        .build()?;
    let credentials = resolve_runtime_credentials(args, &client).await?;

    let body = request_body(args)?;
    let url = format!("{}/responses", credentials.base_url.trim_end_matches('/'));
    if debug {
        eprintln!("debug: POST {url}");
        eprintln!("debug: request body {body}");
    }

    let response = client
        .post(url)
        .bearer_auth(credentials.bearer_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| service(format!("failed to send request to xAI: {e}")))?;

    let status = response.status();
    let value: Value = response
        .json()
        .await
        .map_err(|e| service(format!("failed to parse xAI response as JSON: {e}")))?;

    if !status.is_success() {
        service_bail!("xAI API returned {status}: {}", pretty_json(&value));
    }

    let (text, citations) = extract_response(&value);
    if text.trim().is_empty() && !args.include_response {
        service_bail!(
            "response did not contain output text; rerun with --include-response to inspect the raw response"
        );
    }
    Ok(SearchPostsOutput {
        text: text.trim().to_string(),
        citations,
        response: args.include_response.then(|| pascal_case_keys(value)),
    })
}

fn get_oauth_status(config: &ConfigArgs) -> Result<GetOauthStatusOutput> {
    let auth_file = auth_file_path(config)?;
    let mut output = GetOauthStatusOutput {
        auth_file: auth_file.display().to_string(),
        oauth_configured: false,
        access_token_expiring: false,
    };
    if !auth_file.exists() {
        return Ok(output);
    }
    let auth_store = read_auth_store(&auth_file)?;
    output.oauth_configured =
        xai_access_token(&auth_store).is_ok() && xai_refresh_token(&auth_store).is_ok();
    output.access_token_expiring = xai_access_token(&auth_store)
        .map(|token| access_token_is_expiring(&token, XAI_REFRESH_SKEW_SECONDS))
        .unwrap_or(false);
    Ok(output)
}

async fn start_oauth_login(login_args: &StartOauthLoginArgs) -> Result<StartOauthLoginOutput> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(login_args.timeout_seconds.max(20)))
        .build()?;
    let auth_file = auth_file_path(&login_args.config)?;
    if !login_args.force {
        if let Ok(auth_store) = read_auth_store(&auth_file) {
            if let Ok(access_token) = xai_access_token(&auth_store) {
                if !access_token_is_expiring(&access_token, 60) {
                    eprintln!("Existing xAI OAuth credentials are already available. Run with --force to start a new OAuth login.");
                    return Ok(StartOauthLoginOutput {
                        auth_file: auth_file.display().to_string(),
                        result: LoginResult::AlreadyLoggedIn,
                    });
                }
            }
        }
    }

    let discovery = xai_oauth_discovery(&client).await?;
    let authorization_endpoint = discovery
        .get("authorization_endpoint")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            service("xAI OIDC discovery response was missing authorization_endpoint".to_string())
        })?
        .trim()
        .to_string();
    let token_endpoint = discovery
        .get("token_endpoint")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            service("xAI OIDC discovery response was missing token_endpoint".to_string())
        })?
        .trim()
        .to_string();
    validate_xai_endpoint(&authorization_endpoint)?;
    validate_xai_endpoint(&token_endpoint)?;

    let listener = bind_xai_callback_listener()?;
    let actual_port = listener.local_addr()?.port();
    let redirect_uri = format!(
        "http://{}:{}{}",
        XAI_OAUTH_REDIRECT_HOST, actual_port, XAI_OAUTH_REDIRECT_PATH
    );
    let code_verifier = oauth_pkce_code_verifier();
    let code_challenge = oauth_pkce_code_challenge(&code_verifier);
    let state = random_hex(16);
    let nonce = random_hex(16);
    let authorize_url = xai_oauth_authorize_url(
        &authorization_endpoint,
        &redirect_uri,
        &code_challenge,
        &state,
        &nonce,
    )?;

    eprintln!("Open this URL to authorize x-search with xAI:");
    eprintln!("{authorize_url}");
    eprintln!();
    eprintln!("Waiting for callback on {redirect_uri}");
    eprintln!("If this runs over SSH, forward the callback port to this machine.");

    if login_args.open_browser && !login_args.no_open_browser {
        if webbrowser::open(&authorize_url).is_ok() {
            eprintln!("Browser opened for xAI authorization.");
        } else {
            eprintln!("Could not open a browser automatically; use the URL above.");
        }
    }

    let callback = wait_for_oauth_callback(
        listener,
        XAI_OAUTH_REDIRECT_PATH,
        Duration::from_secs(login_args.timeout_seconds.max(20) * 9),
    )?;

    if let Some(error) = callback.error {
        service_bail!(
            "xAI authorization failed: {}",
            callback.error_description.unwrap_or(error)
        );
    }
    if callback.state.as_deref() != Some(state.as_str()) {
        service_bail!("xAI authorization failed: state mismatch");
    }
    let code = callback
        .code
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(|| {
            service("xAI authorization failed: missing authorization code".to_string())
        })?;

    let token_payload = exchange_xai_oauth_code(
        &client,
        &token_endpoint,
        &code,
        &redirect_uri,
        &code_verifier,
    )
    .await?;

    save_xai_oauth_login(
        &auth_file,
        token_payload,
        json!({
            "authorization_endpoint": authorization_endpoint,
            "token_endpoint": token_endpoint,
        }),
        redirect_uri,
    )?;

    eprintln!("Login successful. Tokens were saved without printing them.");
    Ok(StartOauthLoginOutput {
        auth_file: auth_file.display().to_string(),
        result: LoginResult::LoggedIn,
    })
}

async fn xai_oauth_discovery(client: &reqwest::Client) -> Result<Value> {
    let response = client
        .get(XAI_OAUTH_DISCOVERY_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| service(format!("xAI OIDC discovery failed: {e}")))?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|e| {
        service(format!(
            "failed to parse xAI OIDC discovery response as JSON: {e}"
        ))
    })?;
    if !status.is_success() {
        service_bail!("xAI OIDC discovery returned {status}");
    }
    Ok(value)
}

fn bind_xai_callback_listener() -> Result<TcpListener> {
    TcpListener::bind((XAI_OAUTH_REDIRECT_HOST, XAI_OAUTH_REDIRECT_PORT))
        .or_else(|_| TcpListener::bind((XAI_OAUTH_REDIRECT_HOST, 0)))
        .context("failed to bind xAI OAuth callback listener")
}

#[derive(Default)]
struct CallbackResult {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn wait_for_oauth_callback(
    listener: TcpListener,
    expected_path: &str,
    timeout: Duration,
) -> Result<CallbackResult> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout.max(Duration::from_secs(30));
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let mut buffer = [0_u8; 4096];
                let size = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..size]);
                let first_line = request.lines().next().unwrap_or_default();
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");
                let result = parse_callback_path(path, expected_path);
                let ok = result.as_ref().map(|r| r.error.is_none()).unwrap_or(false);
                write_callback_response(&mut stream, ok)?;
                if let Ok(result) = result {
                    return Ok(result);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error).context("failed while waiting for OAuth callback"),
        }
    }
    service_bail!("xAI authorization timed out waiting for the local callback")
}

fn parse_callback_path(path: &str, expected_path: &str) -> Result<CallbackResult> {
    let url = Url::parse(&format!("http://127.0.0.1{path}"))?;
    if url.path() != expected_path {
        bail!("unexpected OAuth callback path: {}", url.path());
    }
    let mut result = CallbackResult::default();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => result.code = Some(value.into_owned()),
            "state" => result.state = Some(value.into_owned()),
            "error" => result.error = Some(value.into_owned()),
            "error_description" => result.error_description = Some(value.into_owned()),
            _ => {}
        }
    }
    Ok(result)
}

fn write_callback_response(stream: &mut std::net::TcpStream, ok: bool) -> Result<()> {
    let body = if ok {
        "<html><body><h1>x-search authorization received.</h1>You can close this tab.</body></html>"
    } else {
        "<html><body><h1>x-search authorization failed.</h1>You can close this tab.</body></html>"
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    Ok(())
}

fn xai_oauth_authorize_url(
    authorization_endpoint: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
    nonce: &str,
) -> Result<String> {
    let mut url = Url::parse(authorization_endpoint)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", XAI_OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", XAI_OAUTH_SCOPE)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("plan", "generic")
        .append_pair("referrer", "x-search");
    Ok(url.to_string())
}

async fn exchange_xai_oauth_code(
    client: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<Value> {
    let body = serde_urlencoded::to_string([
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", XAI_OAUTH_CLIENT_ID),
        ("code_verifier", code_verifier),
    ])?;
    let response = client
        .post(token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| service(format!("xAI token exchange failed: {e}")))?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|e| {
        service(format!(
            "failed to parse xAI token exchange response as JSON: {e}"
        ))
    })?;
    if !status.is_success() {
        service_bail!("xAI token exchange failed with {status}");
    }
    if value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .is_none()
    {
        service_bail!("xAI token exchange did not return an access_token");
    }
    if value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .is_none()
    {
        service_bail!("xAI token exchange did not return a refresh_token");
    }
    Ok(value)
}

fn save_xai_oauth_login(
    auth_file: &Path,
    token_payload: Value,
    discovery: Value,
    redirect_uri: String,
) -> Result<()> {
    let lock_path = auth_file.with_extension("lock");
    if let Some(parent) = auth_file.parent() {
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

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let auth_store = json!({
        "version": AUTH_STORE_VERSION,
        "updated_at": now,
        "last_refresh": now,
        "auth_mode": "oauth_pkce",
        "redirect_uri": redirect_uri,
        "discovery": discovery,
        "tokens": {
            "access_token": token_payload.get("access_token").cloned().unwrap_or(Value::Null),
            "refresh_token": token_payload.get("refresh_token").cloned().unwrap_or(Value::Null),
            "id_token": token_payload.get("id_token").cloned().unwrap_or_else(|| json!("")),
            "expires_in": token_payload.get("expires_in").cloned().unwrap_or(Value::Null),
            "token_type": token_payload.get("token_type").cloned().unwrap_or_else(|| json!("Bearer")),
        },
    });
    write_auth_store(auth_file, &auth_store)?;
    Ok(())
}

fn oauth_pkce_code_verifier() -> String {
    let mut bytes = [0_u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn oauth_pkce_code_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn random_hex(byte_len: usize) -> String {
    let mut bytes = vec![0_u8; byte_len];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn resolve_runtime_credentials(
    args: &SearchPostsArgs,
    client: &reqwest::Client,
) -> Result<RuntimeCredentials> {
    match args.auth {
        AuthMode::ApiKey => resolve_api_key_credentials(args),
        AuthMode::Oauth => resolve_oauth_credentials(args, client).await,
        AuthMode::Auto => {
            if std::env::var_os(&args.api_key_env).is_some() {
                resolve_api_key_credentials(args)
            } else {
                resolve_oauth_credentials(args, client).await
            }
        }
    }
}

fn resolve_api_key_credentials(args: &SearchPostsArgs) -> Result<RuntimeCredentials> {
    let bearer_token = std::env::var(&args.api_key_env)
        .with_context(|| format!("{} is not set", args.api_key_env))?;
    Ok(RuntimeCredentials {
        bearer_token,
        base_url: selected_base_url(args),
    })
}

async fn resolve_oauth_credentials(
    args: &SearchPostsArgs,
    client: &reqwest::Client,
) -> Result<RuntimeCredentials> {
    let auth_file = auth_file_path(&args.config)?;
    let auth_store = read_auth_store(&auth_file)?;
    let access_token = xai_access_token(&auth_store)?;

    if access_token_is_expiring(&access_token, XAI_REFRESH_SKEW_SECONDS) {
        refresh_xai_oauth_tokens(&auth_file, client).await?;
    }

    let auth_store = read_auth_store(&auth_file)?;
    Ok(RuntimeCredentials {
        bearer_token: xai_access_token(&auth_store)?,
        base_url: selected_base_url(args),
    })
}

async fn refresh_xai_oauth_tokens(auth_file: &Path, client: &reqwest::Client) -> Result<()> {
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
        .map_err(|e| service(format!("failed to refresh xAI OAuth token: {e}")))?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|e| {
        service(format!(
            "failed to parse xAI token refresh response as JSON: {e}"
        ))
    })?;
    if !status.is_success() {
        service_bail!("xAI OAuth token refresh failed with {status}");
    }

    let root = auth_store
        .as_object_mut()
        .ok_or_else(|| anyhow!("auth store is not a JSON object"))?;
    let tokens = root
        .entry("tokens".to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("auth store tokens field is not a JSON object"))?;
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
    root.insert("last_refresh".to_string(), json!(now.clone()));
    root.insert("auth_mode".to_string(), json!("oauth_pkce"));
    root.entry("discovery".to_string())
        .or_insert_with(|| json!({ "token_endpoint": token_endpoint }));
    root.insert("updated_at".to_string(), json!(now));
    root.insert("version".to_string(), json!(AUTH_STORE_VERSION));

    write_auth_store(auth_file, &auth_store)?;
    Ok(())
}

async fn token_endpoint(auth_store: &Value, client: &reqwest::Client) -> Result<String> {
    if let Some(endpoint) = auth_store
        .pointer("/discovery/token_endpoint")
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
        .map_err(|e| service(format!("xAI OIDC discovery failed: {e}")))?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|e| {
        service(format!(
            "failed to parse xAI OIDC discovery response as JSON: {e}"
        ))
    })?;
    if !status.is_success() {
        service_bail!("xAI OIDC discovery returned {status}");
    }
    let endpoint = value
        .get("token_endpoint")
        .and_then(Value::as_str)
        .filter(|endpoint| !endpoint.trim().is_empty())
        .ok_or_else(|| anyhow!("xAI OIDC discovery response was missing token_endpoint"))?;
    Ok(endpoint.trim().to_string())
}

fn selected_base_url(args: &SearchPostsArgs) -> String {
    args.base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .or_else(|| {
            std::env::var("XAI_BASE_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

fn auth_file_path(config: &ConfigArgs) -> Result<PathBuf> {
    let config_dir = config
        .config_dir
        .clone()
        .or_else(|| {
            std::env::var_os("X_SEARCH_CONFIG_DIR")
                .map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
        })
        .or_else(default_config_dir)
        .ok_or_else(|| {
            anyhow!(
                "could not resolve x-search config dir; set --config-dir or X_SEARCH_CONFIG_DIR"
            )
        })?;
    Ok(config_dir.join(AUTH_FILE_NAME))
}

fn default_config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return Some(xdg.join(DEFAULT_CONFIG_SUBDIR));
        }
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".config")
            .join(DEFAULT_CONFIG_SUBDIR)
    })
}

fn read_auth_store(auth_file: &Path) -> Result<Value> {
    let text = fs::read_to_string(auth_file).with_context(|| {
        format!(
            "failed to read x-search auth store at {}",
            auth_file.display()
        )
    })?;
    serde_json::from_str(&text).with_context(|| {
        format!(
            "failed to parse x-search auth store at {}",
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
            .unwrap_or(AUTH_FILE_NAME),
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

fn xai_access_token(auth_store: &Value) -> Result<String> {
    auth_store
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!("xAI OAuth state is missing access_token. Run `x-search auth login` to re-authenticate.")
        })
}

fn xai_refresh_token(auth_store: &Value) -> Result<String> {
    auth_store
        .pointer("/tokens/refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!("xAI OAuth state is missing refresh_token. Run `x-search auth login` to re-authenticate.")
        })
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

fn request_body(args: &SearchPostsArgs) -> Result<Value> {
    let query = args.query.as_str();
    let mut tool = json!({ "type": "x_search" });
    let tool_obj = tool
        .as_object_mut()
        .ok_or_else(|| anyhow!("internal error: tool is not an object"))?;

    if !args.allowed_handles.is_empty() {
        tool_obj.insert(
            "allowed_x_handles".to_string(),
            json!(normalize_handles(&args.allowed_handles)),
        );
    }
    if !args.excluded_handles.is_empty() {
        tool_obj.insert(
            "excluded_x_handles".to_string(),
            json!(normalize_handles(&args.excluded_handles)),
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
            { "role": "system", "content": args.system_prompt },
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

fn extract_response(raw: &Value) -> (String, Vec<String>) {
    let text = raw
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| extract_text_from_output(raw.get("output")))
        .unwrap_or_default();

    let citations = raw
        .get("citations")
        .and_then(Value::as_array)
        .map(|citations| {
            citations
                .iter()
                .map(|citation| match citation {
                    Value::String(url) => url.clone(),
                    other => other
                        .get("url")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| other.to_string()),
                })
                .collect()
        })
        .unwrap_or_default();

    (text, citations)
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

fn text_response(output: &SearchPostsOutput) -> String {
    let mut text = format!("{}\n", output.text);
    if !output.citations.is_empty() {
        text.push_str("\nCitations:\n");
        for citation in &output.citations {
            text.push_str(&format!("- {citation}\n"));
        }
    }
    text
}

fn pascal_case_key(key: &str) -> String {
    key.split(['_', '-'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect()
}

fn pascal_case_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (pascal_case_key(&key), pascal_case_keys(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(pascal_case_keys).collect()),
        value => value,
    }
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
        assert_eq!(extract_response(&value).0, "hello");
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
        assert_eq!(extract_response(&value).0, "hello\nworld");
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

    #[test]
    fn reads_tokens_from_flat_auth_store() {
        let store = json!({
            "version": 1,
            "tokens": {
                "access_token": "access",
                "refresh_token": "refresh"
            }
        });
        assert_eq!(xai_access_token(&store).unwrap(), "access");
        assert_eq!(xai_refresh_token(&store).unwrap(), "refresh");
    }

    #[test]
    fn parser_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn extracts_citation_urls() {
        let value = json!({ "citations": ["https://x.com/a/status/1", { "url": "https://x.com/b/status/2" }] });
        assert_eq!(
            extract_response(&value).1,
            vec!["https://x.com/a/status/1", "https://x.com/b/status/2"]
        );
    }

    #[test]
    fn converts_response_keys_to_pascal_case() {
        assert_eq!(
            pascal_case_keys(json!({ "output_text": "a", "usage": { "input_tokens": 1 } })),
            json!({ "OutputText": "a", "Usage": { "InputTokens": 1 } })
        );
    }
}
