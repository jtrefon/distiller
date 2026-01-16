# Destilation

High-performance, multi-provider agentic distillation system written in Rust.

## Overview

- Generates high-quality datasets for fine-tuning using external model providers.
- Clean architecture with pluggable providers, validators, templates and storage.
- Strong validation and supervision, deduplication and structured persistence.
- CLI-first with optional TUI.

## Quick Start

### Run with defaults

- Default config and parameters are embedded so you can run immediately:

```bash
cargo run --bin destiler
```

- Output is saved to `datasets/default/samples.jsonl`.

### Use an alternative config

- If `config.toml` is present at the repository root, it is loaded automatically.
- Or pass a path explicitly:

```bash
cargo run --bin destiler -- --config path/to/config.toml
```

## Tasks and Workflows

- Submit and run jobs via CLI (default job runs when no options are provided).
- Orchestrator manages tasks and workers, routes to providers and triggers validation and persistence.
- Templates define output structures, prompting, and validation rules.

## Linting

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo llvm-cov --workspace --fail-under-lines 100
cargo audit
```

## Testing

```bash
cargo test --all-targets --all-features
```

## Build

```bash
cargo build --release
```

## Configuration

- By default, the app runs with embedded defaults.
- If `config.toml` exists, runtime settings are loaded (e.g. dataset root, concurrency).
- Future versions will include providers and templates in configuration files:
  - Providers: OpenRouter, Ollama, Mock.
  - Templates: simple, MoE, reasoning, tools and custom.

### Example providers section (config.toml)

```toml
[runtime]
dataset_root = "datasets/algorithms"

[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
model = "openrouter/some-model"

[providers.ollama]
base_url = "http://localhost:11434"
model = "llama3"
```

Set `OPENROUTER_API_KEY` in your environment to enable OpenRouter.

## CI

- GitHub Actions workflow:
  - Format, lint, tests.
  - Extendable to coverage and dependency audit.

## Docs

- Architecture and scope: `SPEC.md`.
- Coding standards: `CODING_STANDARDS.md`.
- Contribution guide: `CONTRIBUTING.md`.
- License: `LICENSE` (Apache 2.0).
