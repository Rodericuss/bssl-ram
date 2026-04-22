# Contributing to bssl-ram

Thanks for taking the time to look under the hood. **bssl-ram** is a small,
focused daemon — the whole thing fits in a handful of files and every change
is measurable on your own machine. That makes contributing pleasant, but it
also means we're strict about keeping the surface area small and the behavior
boring.

This document covers everything you need to send a patch that will actually
get merged.

---

## Table of contents

1. [Code of Conduct](#code-of-conduct)
2. [Before you open anything](#before-you-open-anything)
3. [Ways to contribute](#ways-to-contribute)
4. [Development environment](#development-environment)
5. [Building and running](#building-and-running)
6. [Testing a change](#testing-a-change)
7. [Benchmarks — when required](#benchmarks--when-required)
8. [Code style](#code-style)
9. [Commit messages](#commit-messages)
10. [Pull request process](#pull-request-process)
11. [Adding a new app profile](#adding-a-new-app-profile)
12. [Touching the eBPF program](#touching-the-ebpf-program)
13. [Touching `unsafe` or syscalls](#touching-unsafe-or-syscalls)
14. [Release process](#release-process)
15. [License of contributions](#license-of-contributions)

---

## Code of Conduct

This project adopts the [Contributor Covenant](./CODE_OF_CONDUCT.md).
Participation means you agree to uphold it.

## Before you open anything

- **Search existing [issues](../../issues) and
  [pull requests](../../pulls) first.** A lot of ideas have already been
  discussed — and most rejections come with reasoning worth reading.
- **Security issues never go in public issues.** See
  [SECURITY.md](./SECURITY.md).
- **Keep scope small.** One concern per PR. Refactors that rename things
  should land separately from behavioral changes.
- **Match the project's philosophy.** bssl-ram exists to do *one thing*:
  whisper `MADV_PAGEOUT` to idle renderers. Features that move us toward a
  general-purpose memory manager, a tray app, or a configuration GUI are out
  of scope.

## Ways to contribute

| Type                   | How it gets in                                                       |
|:-----------------------|:---------------------------------------------------------------------|
| Bug report             | Open an issue using the **Bug report** template.                     |
| Performance regression | Use the **Performance regression** template. Include before/after numbers. |
| New app profile        | Use the **Profile request** template *or* open a PR with the rule + a reproduction. |
| Feature proposal       | Open a **Feature request** issue first — don't start coding a 500-line PR before the design discussion. |
| Docs / typos           | Straight PR, no issue needed.                                        |
| Kernel-side tips       | Welcome as README additions or linked documents. Don't ship patches against the kernel from here. |

## Development environment

- **OS**: Linux 5.10+ (process_madvise, pidfd). 6.x recommended for MGLRU.
- **Rust**: stable toolchain pinned in CI — currently 1.94+. Install via
  `rustup`.
- **clang + bpftool**: required for building the eBPF skeleton
  (`src/bpf/cpu_tracker.bpf.c`). `build.rs` generates `vmlinux.h` from
  `/sys/kernel/btf/vmlinux`, so no out-of-tree headers are needed.
- **libbpf** (runtime): loaded via `libbpf-rs`. Your distro package is fine.
- **zram** configured as swap — otherwise changes to the compression path
  can't be meaningfully observed.
- Optional: `perf`, `bpftool prog show`, `bpftrace` for debugging eBPF.

```bash
# Arch
sudo pacman -S rust clang bpf libbpf bpftool zram-generator

# Debian/Ubuntu (22.04+)
sudo apt install rustc cargo clang libbpf-dev bpftool zram-tools
```

## Building and running

```bash
cd daemon
cargo build --release
# Binary lands at target/release/bssl-ram
```

Run against your own user without root:

```bash
sudo setcap cap_sys_nice,cap_sys_ptrace,cap_sys_resource+eip \
    target/release/bssl-ram
./target/release/bssl-ram
```

Or use the systemd template unit under `daemon/systemd/`.

For local iteration prefer `dry_run = true` in
`/etc/bssl-ram/config.toml` — it logs every decision without calling
`process_madvise`.

## Testing a change

Every patch that touches runtime behavior must pass, at minimum:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The repo ships **inspection examples** that bypass the main loop and let you
validate each subsystem in isolation. If your PR changes one of them, show a
run in the description:

```bash
cargo run --example scan_test        # what PIDs the scanner finds
cargo run --example cpu_test         # CPU-tick diffs per target
cargo run --example compress_test    # smaps parsing / region selection
sudo ./target/debug/examples/compress_real   # real MADV_PAGEOUT with RSS delta
```

If you touch the eBPF path, also attach output of:

```bash
sudo ./target/release/bssl-ram 2>&1 | grep -i bpf
sudo bpftool prog show | grep cpu_tracker
```

so reviewers can confirm the skeleton actually loaded on your kernel.

## Benchmarks — when required

PRs that claim performance or memory wins **must** include a reproduction
from `bench/`. See `bench/README.md` for the harness.

Expected artifacts:

- `bench/results/<date>-<topic>/` with the raw JSON/CSV.
- A short before/after table in the PR description (RSS / PSS / swap /
  syscall time — whichever is relevant).
- Hardware + kernel + zram config line (`uname -a`, `cat
  /proc/swaps`, compression-algorithm sysfs).

Hand-wavy "feels faster" claims won't be merged.

## Code style

- **rustfmt** — `cargo fmt --all` before committing. CI enforces it.
- **clippy** — warnings are errors. If you disagree with a lint locally,
  justify the `#[allow(...)]` in a comment.
- **No `unwrap()` / `expect()` on user input paths.** Errors bubble through
  `anyhow::Result`. `unwrap()` in static-init / compile-time-impossible
  branches is fine with a comment.
- **No panics in the scan loop.** A single bad `/proc/PID` must not kill
  the daemon — wrap in `try_` helpers and log at `warn!`.
- **No allocations in the hot path** where we already avoid them (the
  `IOV_MAX` iovec batches in `compressor.rs`, PID tracking keyed maps in
  `state.rs`). Preserve the pattern; don't `Vec::new()` per iteration.
- **Keep `tracing` spans narrow** — one span per syscall batch, not per
  region. Excessive tracing shows up in the very workload we're measuring.
- **Public APIs stay narrow.** Most modules are `pub(crate)`. Don't widen
  visibility unless another crate actually needs it.

## Commit messages

Roughly Conventional-Commits-ish, because the existing history already leans
this way:

```
<type>(<area>): <short summary>

Why: <the one sentence that a future bisector will read>

Refs: #123
```

- Types we use: `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `bench`,
  `chore`, `ci`.
- Areas we use: `daemon`, `scanner`, `compressor`, `bpf`, `psi`, `zram`,
  `config`, `systemd`, `bench`, `ci`, `readme`.
- Prefer small, focused commits. Squash fixup commits before requesting
  review.
- **Never** say what the diff already shows ("add 3 lines to main.rs").
  Explain *why* the change is justified.

Example:

```
perf(scanner): drop full /proc walk, drive scan from cn_proc events

Why: the per-cycle readdir of /proc dominates idle CPU on machines with
>1k tasks. cn_proc gives fork/exec/exit as netlink events so the process
table is maintained incrementally; the timer tick becomes a safety-net.
```

## Pull request process

1. Fork, branch from `main`, push.
2. Open the PR against `main` and fill the template. Link the issue it
   closes with `Closes #N`.
3. CI must be green before review. `ci.yml` runs fmt, clippy, tests and
   builds the BPF skeleton — if it fails for environmental reasons say so
   explicitly in the PR.
4. Expect review to focus on: **measurement** (did you prove the change
   matters?), **scope creep** (can this be two smaller PRs?), **hot-path
   cost** (are we paying for this on idle systems?).
5. Reviewers may push fixup commits or request a rebase. We prefer a rebased,
   linear history on `main`.
6. A maintainer merges once approved. Use *Squash and merge* unless the
   commits are meaningful on their own.

## Adding a new app profile

Profiles live in `daemon/src/config.rs` (defaults) and can be extended via
`/etc/bssl-ram/config.toml`.

For built-in profiles the bar is:

- The app is Linux-native *and* Electron-based / Chromium-based / Firefox-
  based — i.e. its idle renderers accept `MADV_PAGEOUT` without breaking.
- At least one user has observed **>= 20%** RSS reduction on an idle
  renderer with no functional regression.
- The match rule is specific enough that a future version bump of the app
  won't silently start matching its *main* / GPU / network process.

Include in the PR:

- The `argv[0]` substrings and any `--type=` / `-isForBrowser` / `--worker`
  constraints you rely on.
- Exclusion rules (extension processes, crashpad handlers, etc.).
- A `ps -eo pid,args | grep <app>` snippet from your own machine showing
  which PIDs match and which are correctly excluded.
- A `compress_real`-style before/after on one real renderer.

Third-party / niche apps are better shipped as user config examples under
`daemon/examples/configs/` rather than baked in.

## Touching the eBPF program

The BPF program under `daemon/src/bpf/` is **authoritative** for CPU-runtime
tracking (see the v0.3.0 notes). Rules:

- Keep the program CO-RE. Do not introduce architecture-specific offsets.
- Verify your change passes on a fresh kernel (6.x) **and** on an oldish
  one (5.10-ish is the floor). Note your test kernel in the PR.
- Any new map must have a documented size bound and must not grow
  unbounded. Use `BPF_MAP_TYPE_LRU_HASH` for anything PID-keyed.
- The userspace side in `bpf_cpu_tracker.rs` must tolerate load failure
  cleanly — on older kernels / missing CAP_BPF we fall back to
  `/proc/PID/stat` polling. Don't break that fallback.
- Never loosen `PROG_TYPE_TRACING` / `SCHED_*` security requirements to
  "make it work".

## Touching `unsafe` or syscalls

Every `unsafe` block needs a comment explaining *why* it's safe — the
invariant the caller is upholding, why the pointer lifetime is sound, etc.
Syscall wrappers around `process_madvise`, `pidfd_open`, `poll(POLLPRI)`
etc. must:

- Surface `errno` through `anyhow` with context.
- Not panic on expected failures (`ESRCH`, `EPERM`, `ENOSYS` on oldish
  kernels).
- Stay under 100 lines per wrapper — split into helpers before that.

If a change adds a new syscall, mention it in the PR description and in
`SECURITY.md` if it expands the capability requirements.

## Release process

Releases are cut by maintainers. The outline is:

1. Ensure CI on `main` is green and the relevant benches live under
   `bench/results/`.
2. Bump the version in `daemon/Cargo.toml`.
3. Tag `vX.Y.Z`. CI builds and attaches release artifacts.
4. Write release notes that summarize *behavioral* changes — not a dump of
   every commit. Link benches for any perf claim.

## License of contributions

bssl-ram is MIT-licensed (see [LICENSE](./LICENSE)). By submitting a
contribution you agree that it is licensed under the same terms.

If your patch includes code derived from another project, the PR description
must state the origin and the compatible license.
