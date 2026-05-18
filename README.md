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

## OAuth login

Start or reuse an xAI Grok OAuth login:

```sh
x-search auth login
```

On a remote machine, print the URL instead of opening a browser:

```sh
x-search auth login --no-browser
```

Force a fresh OAuth flow even when credentials already exist:

```sh
x-search auth login --force --no-browser
```

Check credential state without printing tokens:

```sh
x-search auth status
```

OAuth state is written to `~/.config/x-search/auth.json`. The location can be overridden with `--config-dir` or `X_SEARCH_CONFIG_DIR`; if neither is set, `$XDG_CONFIG_HOME/x-search` is used when `XDG_CONFIG_HOME` is set.

## Usage

Use OAuth credentials from the auth store:

```sh
x-search "What are people saying about Hermes Agent on X?"
```

Force OAuth explicitly:

```sh
x-search --auth oauth "Hermes Agent"
```

Use an API key explicitly:

```sh
export XAI_API_KEY=...
x-search --auth api-key "Hermes Agent"
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

When using `x-search auth login`, the CLI runs an xAI OAuth loopback flow:

- discovers xAI OAuth endpoints from `https://auth.x.ai/.well-known/openid-configuration`
- starts a local callback listener on `127.0.0.1:56121/callback`, falling back to another local port if needed
- uses PKCE (`S256`), state, and nonce
- opens the authorization URL unless `--no-browser` is passed
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
