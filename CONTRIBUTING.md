# Contributing to Destilation

## 1. Getting Started

- Fork the repository on GitHub.
- Clone your fork locally.
- Ensure a recent stable Rust toolchain is installed.
- Install common tools:
  - cargo fmt
  - cargo clippy
  - cargo audit or equivalent.

## 2. Development Workflow

- Create a feature branch for your work.
- Keep commits focused and logically grouped.
- Run the full local check suite before opening a pull request.

## 3. Local Checks

- Run formatting.
- Run clippy with warnings treated as errors.
- Run unit and integration tests.
- Run coverage tools if available.
- Run audit tools where installed.

## 4. Coding Guidelines

- Follow the rules in the coding standards document.
- Keep public APIs minimal and coherent.
- Prefer introducing abstractions only when they clarify the design.

## 5. Commit Messages and Pull Requests

- Use descriptive commit messages.
- Reference issues when relevant.
- Pull requests should:
  - Explain the problem and solution clearly.
  - Include tests and documentation updates.
  - Describe any user visible changes.

## 6. Code Review Expectations

- Be respectful and constructive in review discussions.
- Reviewers focus on correctness, clarity and maintainability.
- Address review comments with follow up commits or clarifying responses.

## 7. Licensing

- By contributing, you agree that your contributions are licensed under the project license.

