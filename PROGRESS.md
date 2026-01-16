# Destilation Progress Tracker

## Completed

- Spec: architecture and scope documented (SPEC.md)
- Coding standards (CODING_STANDARDS.md)
- Contribution guide (CONTRIBUTING.md)
- License: Apache 2.0 (LICENSE)
- Rust workspace scaffold (core + cli)
- CI: format, clippy, tests, coverage, audit
- Defaults-first config in CLI (auto-load config.toml, else defaults)
- Providers: Mock, OpenRouter, Ollama implementation
- Validators: structural type checks, JSON schema, hash-based and semantic (Jaccard) dedup
- Orchestrator: in-memory job/task stores, filesystem dataset writer, job execution loop
- Routing strategies: capability-based weighted selection
- TUI metrics dashboard: real-time progress monitoring
- README with quick start, lint/test/build, config examples

## In Progress

## Upcoming

- Distributed workers mode
- Storage backends beyond filesystem (S3, GCS)

