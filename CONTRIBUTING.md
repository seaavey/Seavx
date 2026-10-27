# Contributing to Seavx

Thank you for your interest in contributing to Seavx!

## Development Guidelines

1. **Rust Toolchain**: Use the latest stable Rust toolchain.
2. **Formatting & Linting**:
   - Run `cargo fmt --all` before committing.
   - Ensure `cargo clippy --all-targets -- -D warnings` produces zero errors.
3. **Tests**:
   - Run `cargo test` and ensure all unit and integration tests pass.
4. **Code Conventions**:
   - Write all code and code comments in English.
   - Follow YAGNI: avoid unnecessary abstractions or premature generalisations.

## Submitting Pull Requests

1. Fork the repository and create your feature branch.
2. Ensure CI tests pass locally.
3. Submit a Pull Request with a clear English title and description of changes.
