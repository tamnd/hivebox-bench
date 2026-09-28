# Contributing

The rules for code, prose, commits and pull requests are the same as in [hivebox](https://github.com/tamnd/hivebox/blob/main/CONTRIBUTING.md). This document covers what is different about a benchmark harness.

## A new suite

A suite is a line in `src/suite.rs` before it is code. It says what it measures, what it is judged against and which hivebox milestone it needs, and the target has to come from the hivebox spec or be argued for in the pull request. A suite with no target is a suite whose result nobody can call good or bad.

## A result

A report goes under `reports/<date>/` as markdown, with the command that produced it, the hivebox version, the harness commit and the machine. The raw results are attached to the report or linked from it, and they are kept. A report that cannot be recomputed from its raw data does not get merged.

A result that makes hivebox look bad is merged the same way as one that makes it look good. That is the reason this repository exists.

## Running the checks

```
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo nextest run
typos
```

The prose rules are a test in `tests/style.rs`, so `cargo nextest run` covers them.
