# x-search

`x-search` is a small CLI wrapper around the xAI Responses API `x_search` tool. It runs the xAI OAuth loopback login itself and stores its own credentials, so it does not depend on any other tool.

By default it uses:

1. `XAI_API_KEY`, when set.
2. xAI OAuth credentials from `~/.config/x-search/auth.json`, when no API key is set.

## Installation

With `cargo`:

```sh
cargo install --git https://github.com/conao3/rust-x-search
```

With Nix (run the binary directly from the source tree):

```sh
nix shell github:conao3/rust-x-search
```

Or enter the project dev shell and build locally:

```sh
git clone https://github.com/conao3/rust-x-search
cd rust-x-search
nix develop -c cargo install --path .
```

After install, make sure `~/.cargo/bin` is on your `PATH` so `x-search` is found.

## Command line

Commands follow the AWS CLI grammar: `x-search [global options] <command> <subcommand> [--parameter value ...]`.

- Every parameter is named. Lists are space-separated (`--allowed-handles xai grok`). Booleans come in pairs (`--image-understanding` / `--no-image-understanding`). A value starting with `file://` is read from that file, and `file:///dev/stdin` reads standard input.
- `--cli-input-json file://input.json` passes the parameters as a JSON object keyed by the PascalCase parameter names; parameters on the command line take precedence. `--generate-cli-skeleton input|output` prints the input or output skeleton.
- Global options: `--output json|text|table` (default `json`), `--query <JMESPath>`, `--debug`, `--version`.
- `help` works at every level: `x-search help`, `x-search post help`, `x-search post search-posts help`.
- Output keys are PascalCase.
- Exit codes: 0 success, 252 invalid command or parameter, 254 xAI returned an error or could not be reached (search, OAuth discovery, token exchange or refresh, authorization callback), 255 other failures such as missing credentials.

## OAuth login

Start or reuse an xAI Grok OAuth login:

```sh
x-search auth start-oauth-login
```

On a remote machine, print the URL instead of opening a browser:

```sh
x-search auth start-oauth-login --no-open-browser
```

Force a fresh OAuth flow even when credentials already exist:

```sh
x-search auth start-oauth-login --force --no-open-browser
```

The authorization URL and progress messages go to stderr; the result is printed as `{"AuthFile": "...", "Result": "LoggedIn" | "AlreadyLoggedIn"}`.

Check credential state without printing tokens:

```sh
x-search auth get-oauth-status
```

It prints `AuthFile`, `OauthConfigured`, and `AccessTokenExpiring`.

OAuth state is written to `~/.config/x-search/auth.json`. The location can be overridden with `--config-dir` or `X_SEARCH_CONFIG_DIR`; if neither is set, `$XDG_CONFIG_HOME/x-search` is used when `XDG_CONFIG_HOME` is set.

## Usage

Use OAuth credentials from the auth store:

```sh
x-search post search-posts --search-query "What are people saying about Hermes Agent on X?"
```

Force OAuth explicitly:

```sh
x-search post search-posts --auth oauth --search-query "Hermes Agent"
```

Use an API key explicitly:

```sh
export XAI_API_KEY=...
x-search post search-posts --auth api-key --search-query "Hermes Agent"
```

Useful filters:

```sh
x-search post search-posts --allowed-handles xai --from-date 2026-05-01 --search-query "Hermes Agent"
x-search post search-posts --excluded-handles elonmusk --search-query "xAI news"
x-search post search-posts --image-understanding --search-query "AI posts with screenshots"
x-search post search-posts --include-response --search-query "What are people saying about xAI?"
```

The output is `{"Text": "...", "Citations": ["https://x.com/..."]}`; `--include-response` adds the full API response (keys converted to PascalCase) as `Response`. `--output text` prints the answer followed by a `Citations:` list, and `--query Text --output text` prints only the answer.

The CLI sends a single Responses API request with:

- model: `grok-4.3` by default (`--model`)
- system prompt: `--system-prompt`
- tool: `x_search`
- optional filters: `allowed_x_handles` (`--allowed-handles`), `excluded_x_handles` (`--excluded-handles`), `from_date`, `to_date`, `enable_image_understanding`, `enable_video_understanding`

## OAuth behavior

When using `x-search auth start-oauth-login`, the CLI runs an xAI OAuth loopback flow:

- discovers xAI OAuth endpoints from `https://auth.x.ai/.well-known/openid-configuration`
- starts a local callback listener on `127.0.0.1:56121/callback`, falling back to another local port if needed
- uses PKCE (`S256`), state, and nonce
- opens the authorization URL unless `--no-open-browser` is passed
- exchanges the callback code for access and refresh tokens
- writes the result to `~/.config/x-search/auth.json` with owner-only temp-file permissions on Unix

When using `--auth oauth` or the automatic OAuth fallback, the CLI:

- reads tokens from `~/.config/x-search/auth.json` (or `--config-dir`/`X_SEARCH_CONFIG_DIR`)
- refreshes the access token when a JWT `exp` claim is within 120 seconds
- writes refreshed tokens back to `auth.json`
- validates the OAuth token endpoint host before sending a refresh token

The CLI does not print access tokens or refresh tokens.

## Development

```sh
nix develop -c cargo fmt --check
nix develop -c cargo check
nix develop -c cargo clippy -- -D warnings
nix develop -c cargo test
```

## License

Apache-2.0
