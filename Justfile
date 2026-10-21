set shell := ["pwsh", "-NoProfile", "-Command"]

# What running bare `just` does.
default: build

# Formats first. rustfmt is idempotent, needs only a parse, and never changes
# meaning, so it belongs in the loop that runs after every edit rather than in a
# step somebody has to remember. Clippy stays out: its fixes are refactors, and
# it costs a check pass on every build.
#
# Also regenerates the ANTLR lexer and parser: build.rs reruns antlr4-rust-gen
# over grammar/*.g4 whenever a grammar changes.
[doc("Format, then compile the crate and every test target")]
build: fmt
    cargo build --all-targets

# nextest rather than `cargo test` so a panic or stack overflow in one test
# doesn't take the rest of the binary with it; the AST is recursive and the
# corpus nests aggregates several deep. nextest does not run doctests, so the
# second line does: the crate-level example is the one a newcomer pastes.
#
# ARGS go to nextest as written: `just test cvg_pools`, `just test --features gpu`.
# A filterset needs quoting twice, once for just and once for pwsh, which
# would otherwise try to run the parentheses: `just test "-E 'test(/repair/)'"`.
[doc("Run the test suite with nextest, then the doctests")]
test *ARGS:
    cargo nextest run --no-fail-fast {{ARGS}}
    cargo test --doc

[doc("Apply rustfmt; `build` runs this first")]
fmt:
    cargo fmt --all

# Check-only on purpose. `cargo clippy --fix` rewrites code by compiler
# suggestion; those are refactors to read in a diff, not something a build does
# to files another session may be editing. Formatting drift is checked here
# rather than written back, for the same reason. `--all-targets` compiles every
# test, so a test that stops compiling fails here.
[doc("Formatting drift and clippy, warnings denied, check-only")]
lint:
    cargo fmt --all --check
    cargo clippy --all-targets --all-features -- -D warnings

# `--features gpu`: the GPU sieve is opt-in for consumers, but the measurements
# want it, and record "no adapter" honestly on a machine without one.
[doc("Evaluation and constraint-check throughput, in release - a debug number is meaningless here")]
bench:
    cargo nextest run --release --no-capture --features gpu -E 'binary(throughput_benchmarks) | (binary(brute_squad) & test(checks_per_second))'

# Wall-clock budgeted, so they are ignored in debug and only mean anything with
# the machine otherwise idle. See docs/brute-squad.md.
[doc("Time to first feasible point per hit-rate rung, plus checks/s, in release")]
brute:
    cargo nextest run --release --no-capture --no-fail-fast --features gpu --test brute_squad

# Artemis pins sojourn by git tag, so a tag whose version disagrees with
# Cargo.toml resolves fine and wastes an afternoon. The tag is `v<version>`,
# optionally suffixed (`v0.1.3-artemis`); the tree must be clean so the tag
# names what is committed. Tags locally only — pushing is a deliberate step.
# Deterministic per seed, so not wall-clock sensitive like `bench`; release
# only because each reference's kernel sum is quadratic in its twenty
# thousand points. See docs/sampling-quality.md.
[doc("Design quality against closed-form regions, ten seeds a case, in release")]
quality:
    cargo nextest run --release --no-capture --no-fail-fast --test sampling_quality

[doc("Tag the current commit as `v<version>[-suffix]`, checked against Cargo.toml")]
tag name:
    if (git status --porcelain) { throw "the tree is not clean; commit or stash first" }; \
    $manifest = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value; \
    if ("{{name}}" -notmatch "^v$([regex]::Escape($manifest))(-|$)") { throw "Cargo.toml is at $manifest; the tag must be v$manifest or v$manifest-<suffix>, not {{name}}" }; \
    git tag -a "{{name}}" -m "{{name}}"; \
    Write-Host "tagged {{name}}; push it with: git push origin {{name}}"

[doc("Everything CI runs, in CI's order")]
ci: lint build test
