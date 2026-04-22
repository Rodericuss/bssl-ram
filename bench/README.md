<div align="center">

<img src="https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:1a1a2e,100:ff7139&height=160&section=header&text=bench&fontSize=60&fontColor=ff7139&animation=fadeIn&fontAlignY=40&desc=reproducible%20benchmarks%20for%20bssl-ram&descSize=16&descAlignY=62&descColor=e5e7eb" width="100%" alt="header"/>

[![Rust](https://img.shields.io/badge/Rust-1.94+-ce422b?style=for-the-badge&logo=rust&logoColor=fff)](https://www.rust-lang.org/)
[![Bash](https://img.shields.io/badge/bash-5%2B-1a1a2e?style=for-the-badge&logo=gnubash&logoColor=ff7139)](https://www.gnu.org/software/bash/)
[![R](https://img.shields.io/badge/R-ggplot2-276dc3?style=for-the-badge&logo=r&logoColor=fff)](https://cran.r-project.org/)
[![zram](https://img.shields.io/badge/zram-zstd-6e4a7e?style=for-the-badge)](https://wiki.archlinux.org/title/Zram)

**Empirical numbers back every claim in the project [README](../README.md).**

---

*"If it isn't in `bench/`, it didn't happen."*

</div>

---

> [!IMPORTANT]
> Every test is a self-contained shell script plus a TOML config, so
> anyone can re-run the suite on their own machine and get comparable
> numbers. Result files are timestamped; [`analyze.R`](./analyze.R)
> aggregates across every run in [`results/`](./results/) and emits a
> Markdown report plus ggplot PNGs in both dark and light themes.

---

## ⚡ Quick start

Build the daemon first (see top-level [Requirements](../README.md#-requirements)):

```bash
cd ../daemon && cargo build --release
```

Then from `bench/`:

```bash
./scripts/bench-cpu.sh             # ~15 min — three 300s sampling windows
./scripts/bench-psi-latency.sh     # ~2 min  — induces 14 GiB allocation
./scripts/bench-real-compress.sh   # ~1 min  — one-shot on largest target
./scripts/bench-recompression.sh   # ~2 min  — aggressive idle thresholds
```

Each script writes a timestamped file under `results/` and prints a
plain-text summary on stdout. When you're done:

```bash
sudo Rscript -e 'install.packages(c("ggplot2","scales"))'   # one-time, ~2 min
Rscript analyze.R                                           # rebuild tables + plots
Rscript analyze.R --no-plots                                # markdown only
```

---

## 🧪 Tests included

| ID    | Name                   | What it measures                                                                                                  | Script                   |
|:------|:-----------------------|:------------------------------------------------------------------------------------------------------------------|:-------------------------|
| **A** | Discovery CPU          | Daemon CPU consumption across three discovery modes: `/proc` walk, `cn_proc`-only, `cn_proc` + BPF authoritative. | `bench-cpu.sh`           |
| **B** | PSI reaction latency   | Wall-clock time between kernel memory-pressure event and the daemon's next scan, timer-only vs PSI trigger.       | `bench-psi-latency.sh`   |
| **C** | Real compression delta | RSS / PSS / zram delta on the largest renderer, plus per-region `process_madvise` syscall time.                   | `bench-real-compress.sh` |
| **E** | Recompression cascade  | How many compress events fire per unique PID under aggressive idle thresholds — catches regressions in state.rs.  | `bench-recompression.sh` |

---

## 📁 Layout

```
bench/
├── README.md          this file — methodology, how to run, caveats
├── analyze.R          parses results/*.txt across runs → Markdown + ggplot PNGs
├── configs/           per-test daemon TOMLs (drop-in for /etc/bssl-ram/config.toml)
│   ├── A1-procwalk.toml          discovery via /proc walk, BPF off
│   ├── A2-cnproc.toml            cn_proc table on, BPF off
│   ├── A3-cnproc-bpf.toml        cn_proc + BPF authoritative
│   ├── B-psi-on.toml             PSI trigger active
│   ├── B-psi-off.toml            timer-only, same scan_interval
│   └── E-aggressive.toml         tight thresholds for recompression-cascade test
├── scripts/           runners — read configs, setcap binary, collect data
│   ├── bench-cpu.sh              Test A
│   ├── bench-psi-latency.sh      Test B
│   ├── bench-real-compress.sh    Test C
│   └── bench-recompression.sh    Test E
└── results/           output sink — text summaries gitignored, plots committed
    ├── .gitkeep
    └── plots/                   committed PNGs (light + dark variants)
        ├── test-a-cpu-{light,dark}.png
        ├── test-b-psi-latency-{light,dark}.png
        ├── test-c-rss-before-after-{light,dark}.png
        └── test-e-recompression-{light,dark}.png
```

---

## 🔬 Methodology

- **Workload** — whatever browser / Electron processes are open at run
  time. Numbers are most comparable when re-runs happen back-to-back
  with the same window set. Scripts log the live target count
  alongside every measurement so the sample is never blind.

- **Daemon mode** — `dry_run = true` for anything that measures only
  discovery / scheduling overhead. Successive runs aren't perturbed by
  zram churn from a previous round.

- **Capabilities** — each script `setcap`s the binary to the minimum
  needed for the feature under test. A "no cn_proc" run actually
  exercises the `/proc`-walk fallback (no config-flag shortcut).

- **Sampling** — CPU pulled from
  `/proc/<daemon-pid>/stat {utime,stime,cutime,cstime}`. Resolution is
  1 USER_HZ tick (10 ms on default Linux), so long runs sample 300 s at
  a time to stay above the noise floor.

- **Wall-clock metrics** — PSI reaction latency and real compression
  syscall time come from log timestamps emitted by the daemon in the
  `compact` format.

---

## 📊 Aggregation and plotting

`analyze.R` walks every timestamped run in `results/` and produces
means + std-dev, so consecutive invocations naturally build sample
sizes. The PNGs use a brand-aligned theme (Firefox-orange + dark /
light backgrounds) that drops straight into the project README via a
`<picture>` element.

---

## 🧯 Caveats

> [!WARNING]
> - **Capability prompts** — scripts call `sudo setcap` and `sudo cp`
>   and will prompt for the password. Read each before running if you
>   prefer to grant manually.
> - **Live workload variance** — results vary 10–30% across runs
>   because the underlying browser activity is uncontrolled. Run each
>   test several times for a tight confidence interval.
> - **Memory-pressure test (B)** allocates 14 GiB in a child Python
>   process. On a 16 GiB box without swap this can trigger the OOM
>   killer — edit the allocation size in the script if your box is
>   smaller.
> - **Real compression test (C)** actually pages out memory. Run it on
>   a tab you don't mind being briefly paused on next access.

---

## 🧭 See also

- [`../README.md`](../README.md) — project overview and numbers.
- [`../INSTALL.md`](../INSTALL.md) — full install flow.
- [`../daemon/systemd/README.md`](../daemon/systemd/README.md) —
  systemd unit, capability model.
- [`../extension/README.md`](../extension/README.md) — browser-side
  signal protocol.

---

<div align="center">

**`bssl` — if it's not measured, it's a lie.**

</div>
