# Contributing to Reddix

Thank you for your interest in contributing to Reddix. This document explains how to get set up, run the project, and submit changes.

## Development setup

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- Reddit API credentials (client ID, optional client secret) for testing authenticated flows. See [Quickstart](README.md#quickstart) in the main README.

### Build and run

```sh
git clone https://github.com/ck-zhang/reddix.git
cd reddix
cargo build
cargo run
```

Release build:

```sh
cargo build --release
```

### Tests

```sh
cargo test
```

## Code style

- Format code with `cargo fmt`.
- Lint with `cargo clippy` and fix any reported issues (or document why a lint is allowed).
- The project uses Rust 2021 edition and follows common Rust conventions.

Before submitting a PR, run:

```sh
cargo fmt
cargo clippy
cargo test
```

## Submitting changes

1. **Open an issue** (optional but helpful) for larger changes or feature requests, so maintainers and others can discuss the approach.
2. **Fork** the repository and create a branch from `master`.
3. **Make your changes** in small, logical commits with clear messages.
4. **Push** your branch to your fork and open a **Pull Request** against `ck-zhang/reddix` `master`.
5. In the PR description, reference any related issue (e.g. `Fixes #31`) and briefly describe what changed and why.

## Reporting issues

- Use the [GitHub issue tracker](https://github.com/ck-zhang/reddix/issues).
- For bugs, include your environment (OS, terminal, Reddix version) and steps to reproduce.
- For feature ideas, check the [feature request log](docs/feature-requests.md) and existing issues first.

## Questions

- Open a [Discussion](https://github.com/ck-zhang/reddix/discussions) or an issue if something is unclear.

Thanks for contributing.
