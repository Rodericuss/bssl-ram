# Security Policy

**bssl-ram** is a system daemon that holds ambient Linux capabilities
(`CAP_SYS_NICE`, `CAP_SYS_PTRACE`, optionally `CAP_SYS_RESOURCE` and
`CAP_BPF`) and calls `process_madvise(2)` against other processes owned by
the same user. We take security reports seriously and would rather hear
about an issue privately than read about it on a mailing list.

This document covers:

- [Supported versions](#supported-versions)
- [Threat model in plain English](#threat-model-in-plain-english)
- [Reporting a vulnerability](#reporting-a-vulnerability)
- [What qualifies as a vulnerability](#what-qualifies-as-a-vulnerability)
- [What does *not* qualify](#what-does-not-qualify)
- [Scope](#scope)
- [Disclosure policy](#disclosure-policy)
- [Hardening guidance for operators](#hardening-guidance-for-operators)

---

## Supported versions

Only the latest tagged release on `main` receives security fixes.

| Version  | Supported         |
|:---------|:------------------|
| `0.3.x`  | ✅ current         |
| `< 0.3`  | ❌ upgrade please  |

If your distro packages an older version, ping your packager — we don't
backport.

## Threat model in plain English

The daemon's purpose is to tell the kernel "this memory is idle, please
page it out to zram". To do that it:

- Runs as **your user**, not root.
- Holds **`CAP_SYS_NICE`** to pin its own priority.
- Holds **`CAP_SYS_PTRACE`** so `ptrace_may_access()` accepts its
  `pidfd_open(2)` against your own renderers.
- Optionally holds **`CAP_SYS_RESOURCE`** to open `/proc/pressure/memory`
  with `POLLPRI`.
- Optionally holds **`CAP_BPF`** to load the cpu_tracker eBPF skeleton.

Because it targets only PIDs owned by the same UID, the blast radius is
*your own session*. An attacker who can already run code as your user can
already kill those processes, read their memory, and so on. What bssl-ram
intentionally **does not** give an attacker:

- Cross-UID access. We never target a PID whose effective UID differs from
  our own; syscalls would fail anyway.
- Kernel memory access. The eBPF program is CO-RE and uses read-only
  helpers.
- Network reachability. The daemon has no listening socket. There is no
  IPC endpoint, no D-Bus service, no HTTP.
- Persistent state. Configuration is on-disk; runtime state is in-memory
  and dies with the process.

We consider the following a **vulnerability** and want to hear about them:

- Any path that lets a less-privileged process influence bssl-ram into
  touching a PID it shouldn't — e.g. an attacker-controlled cmdline that
  causes us to call `process_madvise` on `init`, a kernel thread, or
  another user's process.
- Any path where the daemon can be made to follow a symlink attack on
  `/proc/PID/*` and end up operating on a different PID than it believed.
- Privilege escalation caused by bssl-ram being on the system (we only
  ever hold ambient caps, so CVEs here usually mean we mis-used them).
- eBPF program paths that leak kernel memory into userspace or bypass the
  verifier's safety assumptions on supported kernels.
- Memory safety issues reachable from `/proc` parsing, smaps parsing, or
  the config loader.
- Authentication/authorization issues in any *future* IPC surface before
  they ship (please catch us early).

## Reporting a vulnerability

**Do not open a public GitHub issue.**

Use one of the following, in order of preference:

1. **GitHub Private Vulnerability Report.** Go to the
   [Security tab](../../security/advisories/new) of this repository and
   open an advisory. This gives us encrypted discussion in-platform.
2. If you cannot use GitHub, write to the maintainer at the email listed
   in the commit history (`git log --format='%ae' | sort -u`) with subject
   prefix `[bssl-ram security]`. Encrypt with the maintainer's published
   PGP key if available.

Please include:

- Affected version or commit SHA.
- Kernel version, distribution, and (if relevant) zram configuration.
- The minimum reproduction — a script, a config, a crafted binary name.
- Your assessment of impact (information disclosure? integrity? DoS?).
- Whether you plan to publish and any timeline pressure (CVE deadlines,
  talks, etc.).

We will acknowledge receipt **within 72 hours** (usually faster) and give
you a first technical response **within 7 days**. If either deadline slips
because of travel/life, we'll say so explicitly rather than go silent.

## What qualifies as a vulnerability

Briefly — the same list from the threat model plus anything where the
impact would make a reasonable operator say *"I wouldn't have installed
it if I'd known"*:

- Memory corruption (unsafe code, FFI, mis-sized buffers).
- Race conditions where a PID gets recycled between `pidfd_open` and
  `process_madvise`, leading us to operate on the wrong task.
- Parsing bugs that cause the daemon to crash-loop under adversarial
  input (this is a DoS of a user-session daemon; we still care).
- Any path where enabling a documented config flag causes an unexpected
  privilege escalation.

## What does *not* qualify

To save everyone time, the following are **out of scope** for the security
program:

- Reports that require the attacker to already have `root` / same-UID
  code execution, unless a *new* privilege is obtained.
- Findings against `MADV_PAGEOUT` itself — that's a kernel interface and
  belongs on [LKML](https://lore.kernel.org/).
- "bssl-ram uses ptrace capability, therefore it's dangerous" without a
  concrete vector.
- Running the daemon with extra capabilities you granted it (e.g. `sudo
  setcap cap_dac_override`) and then discovering it can read things. Don't
  do that.
- Generic Rust supply-chain advisories that we can resolve with a
  `cargo update` — please still tell us, but those are treated as
  regular dependency bumps, not coordinated disclosures.
- Bugs in optional benchmarking scripts under `bench/` that never run in
  production.

## Scope

In scope:

- `daemon/src/**` — daemon binary, all subsystems (scanner, compressor,
  PSI trigger, eBPF cpu_tracker, proc_connector, zram helpers).
- `daemon/build.rs` — BPF skeleton generation.
- `daemon/systemd/**` — unit file and associated capability configuration.

Out of scope:

- Examples under `daemon/examples/*` (they intentionally expose raw
  behavior for debugging).
- Anything under `bench/`.
- Third-party dependencies, except where we make them exploitable through
  our own code.
- Issues that require a modified, non-upstream Linux kernel.

## Disclosure policy

We follow coordinated disclosure:

- Target: **90 days** from the private acknowledgement to public
  disclosure, negotiable shorter (for trivial fixes) or longer (for
  kernel-coupled issues that need distro coordination).
- We will request a CVE through GitHub's advisory workflow when the
  issue warrants one.
- The reporter is credited in the advisory unless they ask otherwise.
- Once a fix is merged and a release is cut, we publish the advisory on
  GitHub and mention it in the release notes. No silent fixes.

## Hardening guidance for operators

Not vulnerabilities, just good operational hygiene:

- Prefer the shipped systemd unit over file capabilities — it narrows
  `NoNewPrivileges`, `ProtectSystem`, `ProtectHome`, `PrivateTmp`, etc.
- Keep `dry_run = true` until you've observed a full workload cycle on
  your machine.
- Audit the `[[profiles]]` section of your config. An over-broad
  `binary_substring_any` is the easiest way to accidentally point the
  daemon at something you didn't intend.
- Run with `RUST_LOG=bssl_ram=debug` when changing configs and read
  `journalctl -u bssl-ram@$USER` — the daemon is loud on purpose about
  which PIDs it accepts and rejects.
- Keep the kernel current. Most of the value here is paid back by the
  kernel (MGLRU, zram, PSI); old kernels leave performance and safety on
  the table.

Thanks for helping keep bssl-ram boring in the best way.
