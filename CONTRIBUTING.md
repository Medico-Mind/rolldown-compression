# Contributing to @medicomind/rolldown-compression

Thanks for your interest in contributing! This document covers everything you need to get a change from idea to merged PR.

## Prerequisites

- **Node.js >= 18** (CI runs on Node 26)
- **Rust** (stable toolchain, with `rustfmt` and `clippy` components) — install via [rustup](https://rustup.rs)

No C toolchain or cmake is needed: the gzip backend is pure-Rust (`zlib-rs`).

## Getting started

```sh
git clone https://github.com/Medico-Mind/rolldown-compression.git
cd rolldown-compression
npm install
npm run build        # release native build + TS bundle
```

For faster iteration during development, use a debug native build:

```sh
npm run build:debug
```

## Project layout

| path | contents |
| --- | --- |
| `src/` | Rust compression core (`lib.rs`, `compress.rs`, `scheduler.rs`), exposed via napi-rs |
| `ts/` | TypeScript plugin source (options normalization, Rolldown plugin, binding loader) |
| `__tests__/unit/` | Vitest unit tests for options and plugin logic |
| `__tests__/integration/` | End-to-end Rolldown build tests (require a native build) |
| `benchmark/` | Benchmark vs `node:zlib` (`npm run bench`) |
| `npm/` | Per-platform binary packages published by the release workflow |
| `.github/workflows/` | CI, changesets versioning, and the napi build/publish matrix |

## Testing

```sh
npm test             # vitest (unit + integration; needs a prior native build)
npm run test:coverage
cargo test           # Rust core tests
npm run typecheck    # tsc --noEmit
COMPRESSION_TEST_LARGE=1 npx vitest run __tests__/integration/large-file.test.ts  # 150 MB asset test
```

Please add tests for new behavior: unit tests for option handling, integration tests for anything that changes emitted assets, and Rust tests for changes to the compression core.

## Linting and formatting

CI enforces all of these, so run them before pushing:

```sh
npm run lint         # biome check
npm run format       # biome check --write (auto-fix)
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Changesets

Releases are driven by [changesets](https://github.com/changesets/changesets). Every PR with a user-facing change (features, fixes, dependency bumps that affect consumers) must include one:

```sh
npx changeset
```

Pick the bump type (`patch` for fixes, `minor` for new options/features) and write a short, user-oriented summary — it becomes the CHANGELOG entry. Docs-only or CI-only changes don't need a changeset.

## Submitting a pull request

1. Fork the repository and create a branch from `main`.
2. Make your change, with tests.
3. Make sure the [test](#testing) and [lint](#linting-and-formatting) commands above pass locally.
4. Add a [changeset](#changesets) if the change is user-facing.
5. Open a PR and fill in the template. CI runs lint plus the test suite on Linux, macOS, and Windows.

Keep PRs focused — one logical change per PR is much easier to review and release.

## Benchmarks

If your change could affect performance (compression core, scheduling, FFI boundary), run the benchmark before and after and include the numbers in the PR description:

```sh
npm run bench
```

Maintainers can also trigger the CI benchmark job manually via `workflow_dispatch` on the CI workflow.

## Release process (maintainers)

Merging PRs with changesets keeps a `chore: release` PR up to date (Version workflow). Merging that PR tags the release, builds the full napi platform matrix, and publishes to npm (Release workflow). A `workflow_dispatch` run of the Release workflow is a dry-run: it builds all platform artifacts without publishing.

## Reporting bugs and requesting features

Please use the [issue templates](https://github.com/Medico-Mind/rolldown-compression/issues/new/choose). If a build problem reproduces without this plugin, report it to [Rolldown](https://github.com/rolldown/rolldown/issues) instead.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](./LICENSE).
