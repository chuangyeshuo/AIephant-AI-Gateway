## Contributing

Thank you for your interest in **Alephant AI Gateway**.

This project is released with a [Contributor Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree to uphold its terms.

## Issues and pull requests

Open an issue for bugs, ideas, or questions. Pull requests are welcome. For large or risky changes, opening an issue first helps align on scope and approach.

## Submitting a pull request

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (edition aligned with the workspace)
- [Docker](https://docs.docker.com/get-docker/) and [Docker Compose](https://docs.docker.com/compose/install/)

### Local setup

1. Fork and clone the repository ([AlephantAI/alephant-ai-gateway](https://github.com/AlephantAI/alephant-ai-gateway)).
2. Copy the environment template and fill in the values you need (see [DEVELOPMENT.md](DEVELOPMENT.md) for common variables):

   ```bash
   cp .env.template .env
   ```

3. Start local dependencies:

   ```bash
   cd infrastructure && docker compose up -d && cd ..
   ```

4. Run the gateway (pick one config; `external` is the usual local choice):

   ```bash
   cargo run -p ai-gateway --features external -- -c ./ai-gateway/config/local.yaml

   # Or a cloud-like local stack:
   cargo run -p ai-gateway --features external -- -c ./ai-gateway/config/local-cloud.yaml
   ```

5. Run checks:

   ```bash
   # Smoke HTTP request helper (see scripts/test)
   cargo run -p test

   # Unit and crate tests (external + integration harness where applicable)
   cargo test -p ai-gateway --tests --features "external integration"
   ```

6. Format Rust changes with nightly rustfmt (matches repo hooks):

   ```bash
   cargo +nightly fmt
   ```

7. Create a branch, commit with a clear message, push to your fork, and open a PR against the default branch.

We follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) where practical.

### What makes a PR easier to merge

- Focused changes with tests where behavior changes.
- Notes on how you validated the change (commands, configs, feature flags `external` vs `internal`).
- Updates to user-facing docs when behavior or configuration changes.

Work-in-progress PRs are fine when you want early feedback.

## More detail

See [DEVELOPMENT.md](DEVELOPMENT.md) for day-to-day commands and [AGENTS.md](AGENTS.md) for repository conventions used by tooling and collaborators.
