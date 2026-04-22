<!--
Thanks for sending a PR to bssl-ram!

Fill in the sections below. You can delete any section that truly doesn't
apply (e.g. "Benchmarks" for a docs-only change), but please leave the
checklist at the bottom — reviewers use it.

One concern per PR. If you caught yourself writing "also fixed X" below,
consider splitting into two PRs.
-->

## Summary

<!-- One or two sentences: what does this PR change, and why?
     The *why* belongs here, not in the code comments. -->

## Type of change

<!-- Mark with [x] one or more. -->

- [ ] `fix` — behavior bug
- [ ] `feat` — new user-visible capability (config flag, profile, syscall path)
- [ ] `perf` — measurable performance / memory improvement
- [ ] `refactor` — no behavioral change
- [ ] `docs` — README, inline docs, this very file
- [ ] `test` / `bench` — improves coverage or reproducibility
- [ ] `chore` / `ci` — build, tooling, release plumbing

## Related issues

<!-- e.g. "Closes #123", "Refs #456". If no issue exists and this is
     non-trivial, please open one and link it. -->

Closes #

## What the user will observe

<!-- How does this change show up to someone running `bssl-ram` on their
     machine? Example:
     - New config key `psi_window_us` defaulting to 1_000_000 µs.
     - compress_real now prints per-region syscall time.
     - Scanner no longer matches `--type=gpu-process`. -->

## Design notes

<!-- Anything a reviewer shouldn't have to reverse-engineer:
     - Why this approach over the alternatives you considered.
     - Invariants the code now relies on.
     - Interaction with PSI / eBPF / cn_proc that isn't obvious. -->

## Testing

<!-- Required for anything that isn't pure docs. Paste real commands and
     their output (trim long logs). At minimum: -->

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

<!-- Plus, where relevant: -->

- [ ] `cargo run --example scan_test` matches expected PIDs on my machine.
- [ ] `cargo run --example cpu_test` shows realistic deltas.
- [ ] `cargo run --example compress_test` parses smaps correctly.
- [ ] `sudo ./target/debug/examples/compress_real` shows sane before/after
  RSS / PSS / swap.

**Host / kernel / zram:**

```text
<!-- paste `uname -a`, `cat /proc/swaps`, and the relevant
     /sys/block/zram0/comp_algorithm line -->
```

## Benchmarks

<!-- Required for PRs tagged `perf`. Drop the link to the run under
     bench/results/ and include a short delta table. For non-perf PRs,
     keep this section only if you have numbers worth sharing.

     | Metric   | Before | After | Δ    |
     |:---------|-------:|------:|-----:|
     | RSS      |        |       |      |
     | PSS      |        |       |      |
     | Swap     |        |       |      |
     | Syscall  |        |       |      |
-->

## Capabilities / syscalls touched

<!-- Fill in if this PR adds, widens, or narrows Linux capability use or
     introduces a new syscall. Otherwise write "none".

     Example:
     - Adds `CAP_BPF` (was implicit via `CAP_SYS_ADMIN`).
     - New syscall: `pidfd_send_signal(2)` — rationale here.
-->

## Breaking changes

<!-- Does this change:
     - the on-disk config schema?
     - the systemd unit contract?
     - example outputs third parties might depend on?
     If yes, describe and propose a migration path. -->

- [ ] This PR is fully backward-compatible.
- [ ] This PR is a breaking change and I describe the migration above.

## Reviewer checklist

<!-- Keep these checked honestly. An unchecked box is not a blocker — it
     tells the reviewer where to spend attention. -->

- [ ] One concern per PR.
- [ ] `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` all pass locally.
- [ ] New `unsafe` blocks have a comment explaining the invariant.
- [ ] Hot-path allocations / new syscalls on idle systems are justified.
- [ ] Tracing spans are proportional to the work (no per-region log spam).
- [ ] Docs / README / CONTRIBUTING updated if user-visible behavior changed.
- [ ] No secrets, no machine-specific paths, no personal IDs in the diff.
- [ ] I've re-read the diff with `git diff --stat` and it's the minimum
  needed to achieve the goal.
