# Destilation Coding Standards

## 1. Goals

- Maintain a clean and consistent codebase.
- Support long-term evolution without uncontrolled complexity.
- Enable contributors to reason about behavior quickly and safely.

## 2. Language and Style

- Implementation language is Rust.
- Formatting
  - All code formatted using rustfmt with default stable configuration.
- Naming
  - Descriptive and intention-revealing names.
  - Avoid unclear abbreviations.
- Structure
  - Small, focused modules and types.
  - Keep public interfaces minimal and stable.

## 3. Design Principles

### 3.1 SOLID

- Single responsibility
  - Each module and type has one responsibility.
- Open closed design
  - Extend behavior via traits and new types.
  - Avoid modifying core logic for new providers or validators.
- Liskov substitution
  - Implementations of traits must behave consistently and predictably.
- Interface segregation
  - Prefer smaller traits focused on one concern.
- Dependency inversion
  - Core logic depends on abstractions such as traits.
  - Adapters implement those abstractions at the edges.

### 3.2 DRY and SRP

- Do not duplicate logic across modules.
- Extract shared utilities when behavior is reused.
- Keep responsibilities clearly separated, for example:
  - Orchestration.
  - Provider integration.
  - Validation.
  - Persistence.
  - UI.

### 3.3 YAGNI and KISS

- Implement only what is required by the current specification.
- Avoid speculative abstractions and configurations.
- Prefer explicit, straightforward control flow.

## 4. Error Handling

- Use Result based error handling.
- Use dedicated error types per module.
- Propagate errors with context so failures are diagnosable.

## 5. Testing

- Aim for 100 percent unit test coverage on production code.
- Treat tests as first class citizens.
- Write tests for:
  - Core domain logic.
  - Template behavior.
  - Validation behavior.
  - Provider integration with mocks.
- Favor small, focused tests and avoid brittle coupling to implementation details.

## 6. Static Analysis and Tooling

- Use rustfmt and clippy in development.
- Treat clippy warnings as errors in CI wherever practical.
- Use cargo audit and related tools to monitor dependency health.

## 7. Concurrency and Performance

- Prefer async Rust for I O and external calls.
- Avoid unnecessary blocking operations in async contexts.
- Measure and reason about performance when adding new layers of abstraction.

## 8. Logging and Observability

- Use structured logging with clear context such as job and task identifiers.
- Do not log secrets or sensitive data.
- Use consistent log levels.

## 9. Code Review

- All changes flow through pull requests.
- Requirements before merge:
  - Green CI.
  - At least one approving review.
  - Tests and documentation updated when behavior changes.

## 10. Technical Debt

- Strict 0 debt policy.
- No commented-out code is allowed in the codebase.
  - If code is obsolete, delete it. Git history preserves it.
  - If code is for future use, implement it behind a feature flag or keep it in a separate branch.
- No TODOs or FIXMEs in the codebase without a tracking issue or immediate plan to resolve.

