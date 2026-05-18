# x-search

`x-search` is a small CLI wrapper around the xAI Responses API `x_search` tool. It extracts the useful core of Hermes X Search into a standalone command that only needs an xAI API key.

## Requirements

- `XAI_API_KEY` with access to the xAI Responses API and X Search tool
- Rust toolchain from the project Nix shell

## Usage

```sh
export XAI_API_KEY=...
nix develop -c cargo run -- "What are people saying about Hermes Agent on X?"
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

## Development

```sh
nix develop -c cargo fmt --check
nix develop -c cargo check
nix develop -c cargo clippy -- -D warnings
```

## License

Apache-2.0
