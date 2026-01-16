# Destilation – Architecture and Scope

## 1. Project Overview

### 1.1 Vision

Destilation is an open-source, Rust-based, Linux-targeted system for agentic model distillation. It orchestrates multiple external LLM providers (OpenRouter, Ollama-compatible local APIs, and future plugins) to generate high-quality, structured datasets for fine-tuning.

### 1.2 Objectives

- High-throughput, multi-provider distillation pipeline with strong supervision.
- Support for:
  - Simple outputs.
  - Reasoning-heavy or chain-of-thought outputs.
  - Mixture-of-experts (MoE) configurations.
  - Tool-using traces.
- Output template system with built-in and custom formats.
- Strong quality, validation, and deduplication layers.
- First-class CLI and TUI for observability and control.
- Top-tier engineering standards:
  - Clean architecture, SOLID, DRY, SRP, YAGNI, KISS.
  - Strict coding standards enforced in CI.
  - Static analysis and 100 percent unit-test coverage on production code.
  - GitHub CI/CD pipeline for build and release.

### 1.3 Targets

- Runtime: Linux.
- Development: macOS and Linux.
- Distribution: GitHub releases (binaries and Docker images).

---

## 2. Core Domain Model

### 2.1 Jobs, Tasks and Samples

- Job
  - High-level distillation campaign.
  - Defines domains, output templates, provider mix, validation rules and target dataset size.
- Task
  - Individual generation unit associated with a job.
  - Represents a single sample to be generated, validated and persisted.
- Sample
  - Validated output stored in the dataset.
  - Conforms to a specific output template and associated schema.

### 2.2 Agents

- Orchestrator
  - Manages jobs, schedules tasks and coordinates workers and validation.
  - Ensures no duplicate work and tracks global progress.
- Workers
  - Execute tasks by calling model providers in parallel.
  - Use templates to construct prompts and interpret responses.
- Validation agent
  - Validates outputs structurally, syntactically and semantically.
  - Detects corruption, duplication and standard violations.
  - Provides structured feedback for retry loops.
- Persistence agent
  - Writes validated samples to durable storage in the configured format.
  - Maintains dataset metadata and indices.

---

## 3. Templates and Distillation Modes

Destilation uses a template system to define the structure, style and semantics of outputs.

### 3.1 Template Types

- Simple template
  - Direct, concise outputs such as Q and A or instructions.
- MoE template
  - Outputs encoding domain or expert labels and possibly multiple expert-style answers.
- Reasoning template
  - Multi-step reasoning or chain-of-thought.
- Tool-trace template
  - Captures reasoning and tool calls for tool-using models.
- Custom template
  - User-defined structures backed by an explicit schema.

### 3.2 Template Definition

Each template defines:

- Identifier, name and description.
- Mode: simple, moe, reasoning, tools or custom.
- Schema:
  - Fields, types and constraints.
  - Required and optional fields.
- Serialization policy:
  - JSON-encoded objects.
  - JSONL dataset output, one sample per line.
- Prompting specification:
  - System message describing distillation goals.
  - User prompt pattern.
  - Optional few-shot examples.
- Validation configuration:
  - Structural and semantic validators for this template.
  - Additional rules such as reasoning length and tool trace correctness.

### 3.3 Template Configuration

- Templates are configured via repository config files.
- Jobs reference templates by identifier.
- Users can:
  - Use built-in templates.
  - Add custom templates with custom schemas.
  - Override or extend validation rules per template.

---

## 4. Architecture

### 4.1 Architectural Style

- Clean or hexagonal architecture.
- Core domain is independent of HTTP clients, storage engines and UI.
- Ports and adapters:
  - Ports: model providers, validators, storage and UI.
  - Adapters: OpenRouter provider, Ollama provider, filesystem storage, terminal UI and similar.
- Key patterns:
  - Strategy for provider selection and validation policies.
  - Factory for job, template and validator instantiation.
  - Pipeline for validation stages.
  - Command for job execution.

### 4.2 Components

- CLI layer
  - Commands: run, list-jobs, show-job, resume and related commands.
  - Parses configuration and starts the orchestrator.
- Orchestrator
  - Manages task queues per job.
  - Applies routing strategies to providers.
  - Tracks job and task states in the state store.
- Worker pool
  - Async workers executing tasks.
  - Each worker:
    - Builds prompt from template.
    - Calls providers.
    - Passes responses to validation.
- Validation pipeline
  - Sequence of validators:
    - Structural JSON and schema validation.
    - Content rules.
    - Deduplication checks.
    - Optional LLM-based validation or scoring.
- Persistence layer
  - Writes JSONL datasets and maintains metadata and indices.
  - Uses a pluggable storage interface.
- State store
  - Database, initially SQLite, persisting job, task and metrics state.
- TUI or UI layer
  - Provides real-time visualization and control in the terminal.

### 4.3 Execution Model

- Single process, multithreaded async runtime in the initial version.
- Bounded internal queues for tasks and results.
- Future option to run workers as separate processes or services.

---

## 5. Providers and Plugins

### 5.1 Model Providers

- Model providers expose:
  - Metadata such as supported models, capabilities and token limits.
  - Async generation function accepting structured prompts.
- Built-in providers:
  - OpenRouter provider.
  - Ollama provider.
  - Mock provider for testing.

### 5.2 Multi-Provider Execution

- Provider selection strategies:
  - Round-robin.
  - Weighted random.
  - Capability-based routing.
- Multiple providers active simultaneously for throughput and data diversity.

### 5.3 Plugin Mechanism

- Phase one:
  - Providers compiled into the binary and configured via type names.
- Phase two:
  - External provider plugins communicating over a simple protocol.
  - Optional WASM-based providers.

---

## 6. Validation and Supervision

### 6.1 Validation Pipeline

- Structural validator
  - Ensures valid JSON and schema conformity.
- Syntactic validator
  - Detects truncation and corruption.
- Semantic or rule-based validators
  - Domain rules and quality rules.
- Deduplication validator
  - Hash-based exact duplicate detection.
  - Optional semantic similarity based detection.
- LLM-based supervisor
  - Optional stronger model used to vet or score samples.

### 6.2 Feedback and Retries

- On failure:
  - Structured validation outcome with issues and hints.
  - Worker re-prompts with feedback.
  - Retries limited by per-job maximum attempts.
- On repeated failure:
  - Task marked rejected.
  - Reasons recorded for analysis.

---

## 7. TUI Specification

### 7.1 Approach

- Cross-platform terminal user interface.
- Implemented using a modern Rust TUI library and ASCII or Unicode enriched layout.
- TUI is optional but first-class. CLI continues to work without it.

### 7.2 Views

- Main dashboard
  - Global status of jobs.
  - System throughput and provider health.
  - Validation pass rate and error rates.
- Job detail view
  - Progress bars.
  - Counts per state.
  - Domain, template and provider breakdowns.
- Task and agent view
  - Worker states and per-agent throughput.
  - Validation statistics and common failures.
- Logs and events
  - Filterable log window with job, provider and severity filters.

### 7.3 Interactions

- Keyboard-driven navigation.
- Actions:
  - Pause or resume jobs.
  - Inspect individual tasks.
  - Toggle detail level and filters.

---

## 8. Quality, Testing and Static Analysis

### 8.1 Testing Strategy

- Unit tests
  - Target of 100 percent coverage on production code, excluding generated code.
  - Cover core domain logic, templates, providers, validation and persistence.
- Integration tests
  - End to end orchestration, worker, validation and persistence flows.
  - Tests for multi-provider runs and job resumption.
- Property-based tests
  - For critical parsers and scheduling logic where useful.

### 8.2 Coverage and Static Analysis

- Coverage
  - Coverage tools integrated into CI.
  - Builds fail if coverage falls below agreed thresholds.
- Static analysis
  - Formatting enforced by rustfmt.
  - Linting enforced by clippy with most warnings treated as errors.
  - Dependency checks using common Rust audit tooling.

---

## 9. CI and CD

### 9.1 Continuous Integration

- GitHub Actions workflow for pull requests and pushes to main.
- Steps:
  - Format check.
  - Linting.
  - Unit and integration tests.
  - Coverage and audit checks.
- All checks must pass before merging.

### 9.2 Continuous Delivery and Releases

- Tagged releases:
  - Build release binaries for Linux and macOS.
  - Build and publish Docker images.
  - Attach binaries to GitHub releases.
- Semantic versioning:
  - Versioning follows major, minor and patch semantics.

---

## 10. Licensing

- License: Apache License 2.0.
- Rationale:
  - Common for infrastructure and AI projects.
  - Includes explicit patent grant and contribution terms.
  - Supports both open and commercial usage.

---

## 11. Roadmap Summary

- Phase one
  - Core MVP with clean architecture.
  - Built-in templates and providers.
  - Basic validation pipeline and persistence.
  - Basic TUI dashboard.
  - CI with formatting, linting, tests, coverage and audit.
- Phase two
  - Provider plugins.
  - Advanced validators and semantic deduplication.
  - Enhanced TUI views.
  - Additional storage backends.
- Phase three
  - Distributed workers.
  - Integration with training pipelines.
  - Optional web dashboard.
