# Destilation – High-Performance Agentic Distillation

## 1. Project Overview

### 1.1 Vision

Destilation is a high-performance, Rust-based system designed to generate massive, high-quality datasets for LLM fine-tuning. It orchestrates multiple providers (OpenRouter, Ollama, etc.) to distill expert model knowledge into structured formats, supporting complex reasoning, tool usage, and domain-specific traces.

### 1.2 Core Objectives

- **High-Throughput Orchestration**: Parallel execution across multiple providers with intelligent rate-limiting and scheduling.
- **Multi-Modal Distillation**:
  - **Simple Q&A**: Standard instruction-following pairs.
  - **Reasoning**: Detailed Chain-of-Thought (CoT) traces for complex problem solving.
  - **Tool-Enabled**: Agentic traces showing reasoning followed by tool calls and environment responses.
  - **Coding**: Code generation with or without explanatory reasoning steps.
- **Strict Quality Control**: Multi-layer validation (structural, syntactic, semantic) and mandatory deduplication.
- **Observability**: Real-time throughput tracing and progress monitoring via a first-class TUI.
- **Zero-DB Architecture**: Stateless core with file-based configuration and directory-centric dataset management.

---

## 2. Core Domain Model

### 2.1 Jobs and Samples

- **Job**: A single distillation campaign defined in a `config.toml` or passed via CLI.
  - **Domain**: The subject area (e.g., "Quantum Physics", "Rust Systems Programming").
  - **Target Size**: Specific count of samples to generate.
  - **Template**: The structural blueprint for the output.
  - **Provider Mix**: Distribution of work across different model backends.
- **Sample**: A single validated unit of the dataset.
  - Must conform to the template schema.
  - Persisted as a JSON object in a `.jsonl` file.

### 2.2 Orchestration Flow

1. **Initialization**: Load configuration and existing dataset state from the target folder.
2. **Task Generation**: Orchestrator generates task units based on the target count and domain.
3. **Worker Execution**: Parallel workers pull tasks, select providers, and execute generation using the job's template.
4. **Supervised Validation**:
   - **Structural**: Validates JSON and schema conformity.
   - **Semantic**: Checks for exact and fuzzy duplicates.
   - **Recovery**: Failed samples trigger a retry loop with structural feedback provided back to the model.
5. **Persistence**: Validated samples are appended to the dataset file in real-time.

---

## 3. Template System

Destilation uses a rigorous template system to ensure dataset consistency.

### 3.1 Built-in Templates

- **simple_qa**: `{"question": "...", "answer": "..."}`
- **reasoning**: `{"question": "...", "reasoning": "...", "answer": "..."}`
- **tool_trace**: `{"thought": "...", "tool_call": "...", "tool_response": "...", "final_answer": "..."}`
- **coding**: `{"requirement": "...", "code": "...", "explanation": "..."}`
- **coding_reasoning**: `{"requirement": "...", "reasoning": "...", "code": "..."}`

### 3.2 Custom Templates

- Users define custom templates in `config.toml`.
- **Definition**:
  - `id`: Unique identifier.
  - `schema`: A list of required fields and their types.
  - `system_prompt`: Instructions for the model.
  - `user_prompt_pattern`: A template string with placeholders like `{{domain}}`.
- **Introspection**: The CLI/TUI must be able to "show" the content/schema of any template so users can reference them for custom building.

---

## 4. Architecture & Storage

### 4.1 File-Based Orchestration

- **Configuration**: All global and job-specific settings live in `config.toml`.
- **Storage**:
  - **Datasets**: Extracted to specific folders (e.g., `./datasets/job-name/`).
  - **Metadata**: A `metadata.json` or `.events.jsonl` in the dataset folder tracks progress, throughput, and validator statistics.
- **No Database**: SQLite or other relational DBs are explicitly excluded to avoid unnecessary complexity and state drift.

### 4.2 Components

- **Core**: Domain logic, templates, and validation pipeline.
- **Providers**: Adapters for OpenRouter (SaaS) and Ollama (Local).
- **CLI**: The main entry point for running jobs and managing templates.
- **TUI**: Real-time dashboard with:
  - Total samples vs. Target.
  - Durchsatz (samples/min).
  - Validation pass/fail rates.
  - Real-time event log.

---

## 5. Quality & Validation

### 5.1 Deduplication

- **Hash-based**: Prevents exact string matches.
- **Semantic**: Jaccard similarity or embedding-based checks to prevent redundant data points.

### 5.2 Error Recovery

- Workers detect model hallucinations or structural errors.
- **Retry Mechanism**: The system re-prompts the model, including the specific validation error, up to a configurable `max_attempts`.

---

## 6. Verification Requirements

- **Unit Tests**: 100% coverage on core logic and template rendering.
- **Integration Tests**: End-to-end dry runs using a `MockProvider`.
- **Performance**: Capable of sustaining high-throughput generation without memory leaks or file handle exhaustion.
