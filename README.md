# x-search

`x-search` is a small CLI wrapper around the xAI Responses API `x_search` tool. It extracts the useful core of Hermes X Search into a standalone Rust command.

By default it uses:

1. `XAI_API_KEY`, when set.
2. Hermes xAI OAuth credentials from `~/.hermes/auth.json`, when no API key is set.

That means a machine already logged in through Hermes xAI OAuth can use `x-search` without copying an API key.

## Requirements

- Rust toolchain from the project Nix shell
- Either:
  - `XAI_API_KEY` with access to the xAI Responses API and X Search tool, or
  - an existing Hermes xAI OAuth login (`hermes model` → xAI Grok OAuth)

## Usage

Use the existing Hermes xAI OAuth login:

```sh
nix develop -c cargo run -- "What are people saying about Hermes Agent on X?"
```

Force Hermes OAuth explicitly:

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

## Hermes OAuth behavior

When using `--auth hermes-oauth` or the automatic Hermes fallback, the CLI:

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
