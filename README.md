# x-search

`x-search` is a small CLI wrapper around the xAI Responses API `x_search` tool. It extracts the useful core of Hermes X Search into a standalone Rust command.

By default it uses:

1. `XAI_API_KEY`, when set.
2. Hermes-compatible xAI OAuth credentials from `~/.hermes/auth.json`, when no API key is set.

The CLI can also run the xAI OAuth loopback login itself, so it does not require Hermes to be installed after setup.

## Requirements

- Rust toolchain from the project Nix shell
- Either:
  - `XAI_API_KEY` with access to the xAI Responses API and X Search tool, or
  - xAI OAuth credentials created by `x-search auth login` or Hermes (`hermes model` → xAI Grok OAuth)

## OAuth login

Start or reuse an xAI Grok OAuth login:

```sh
nix develop -c cargo run -- auth login
```

On a remote machine, print the URL instead of opening a browser:

```sh
nix develop -c cargo run -- auth login --no-browser
```

Force a fresh OAuth flow even when credentials already exist:

```sh
nix develop -c cargo run -- auth login --force --no-browser
```

Check credential state without printing tokens:

```sh
nix develop -c cargo run -- auth status
```

OAuth state is written to `~/.hermes/auth.json` under `providers.xai-oauth`, matching the Hermes auth-store shape.

## Usage

Use OAuth credentials from the auth store:

```sh
nix develop -c cargo run -- "What are people saying about Hermes Agent on X?"
```

Force OAuth explicitly:

```sh
nix develop -c cargo run -- --auth hermes-oauth "Hermes Agent"
```

Use an API key explicitly:

```sh
export XAI_API_KEY=...
nix develop -c cargo run -- --auth api-key "Hermes Agent"
```

Useful filters:

```sh
x-search --allow-handle xai --from-date 2026-05-01 "Hermes Agent"
x-search --exclude-handle elonmusk "xAI news"
x-search --image-understanding "AI posts with screenshots"
x-search --json "What are people saying about xAI?"
```

The CLI sends a single Responses API request with:

- model: `grok-4.3` by default
- tool: `x_search`
- optional filters: `allowed_x_handles`, `excluded_x_handles`, `from_date`, `to_date`, `enable_image_understanding`, `enable_video_understanding`

## OAuth behavior

When using `x-search auth login`, the CLI mirrors Hermes' xAI OAuth loopback flow:

- discovers xAI OAuth endpoints from `https://auth.x.ai/.well-known/openid-configuration`
- starts a local callback listener on `127.0.0.1:56121/callback`, falling back to another local port if needed
- uses PKCE (`S256`), state, and nonce
- opens the authorization URL unless `--no-browser` is passed
- exchanges the callback code for access and refresh tokens
- writes the result to `~/.hermes/auth.json` with owner-only temp-file permissions on Unix

When using `--auth hermes-oauth` or the automatic OAuth fallback, the CLI:

- reads `xai-oauth` tokens from `~/.hermes/auth.json` or `--hermes-home`/`HERMES_HOME`
- refreshes the access token when a JWT `exp` claim is within 120 seconds
- writes refreshed tokens back to `auth.json` under the same `xai-oauth` provider state
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
