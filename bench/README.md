# bench/ — reproducible benchmarks for bssl-ram

Empirical measurements that back the numbers in the project README.
Every test is a self-contained shell script + TOML config so anyone can
re-run the suite on their own machine and get comparable results.

## Layout

```
bench/
├── README.md          (this file — methodology, how to run, caveats)
├── analyze.R          (parses results/*.txt across runs → markdown + ggplot PNGs)
├── configs/           (per-test daemon TOMLs, drop-in for /etc/bssl-ram/config.toml)
│   ├── A1-procwalk.toml          discovery via /proc walk, BPF off
│   ├── A2-cnproc.toml            cn_proc table on, BPF off
│   ├── A3-cnproc-bpf.toml        cn_proc + BPF authoritative
│   ├── B-psi-on.toml             PSI trigger active
│   ├── B-psi-off.toml            timer-only, same scan_interval
│   └── E-aggressive.toml         tight thresholds for recompression-cascade test
├── scripts/           (runners — read configs, set caps, collect data)
│   ├── bench-cpu.sh              Test A — daemon CPU consumption per discovery mode
│   ├── bench-psi-latency.sh      Test B — reaction time under induced memory pressure
│   ├── bench-real-compress.sh    Test C — RSS / zram delta on the largest renderer
│   └── bench-recompression.sh    Test E — count compress events vs unique PIDs
└── results/           (output goes here — text summaries gitignored, plots checked in)
    ├── .gitkeep
    └── plots/                   committed PNGs (light + dark theme variants)
        ├── test-a-cpu-{light,dark}.png
        ├── test-b-psi-latency-{light,dark}.png
        ├── test-c-rss-before-after-{light,dark}.png
        └── test-e-recompression-{light,dark}.png
```

## Aggregating + plotting

After running any of the scripts, regenerate the markdown report + PNGs with:

```bash
sudo Rscript -e 'install.packages(c("ggplot2","scales"))'   # one-time, ~2 min
Rscript bench/analyze.R                                     # rebuilds tables + plots
Rscript bench/analyze.R --no-plots                          # markdown only
```

The script aggregates **across every timestamped run currently in
`results/`**, so consecutive invocations build sample sizes that show up
as means + std-dev in the output table. The PNGs use a brand-aligned
theme (Firefox-orange + dark/light backgrounds) so they drop straight
into the project README via a `<picture>` element.

## Methodology

* **Workload**: whatever browser/electron procs are open at the time of
  the run. Numbers are most comparable when re-runs happen back-to-back
  with the same set of windows. The scripts log the live target count
  alongside every measurement.

* **Daemon mode**: `dry_run = true` for any test that only measures
  discovery / scheduling overhead, so successive runs aren't perturbed
  by zram churn from a previous round.

* **Capabilities**: each script `setcap`s the binary to the minimum
  needed for the feature being tested, so a "no cn_proc" run actually
  exercises the /proc-walk fallback (rather than relying on a config
  flag that wouldn't reflect a real-world install).

* **Sampling**: CPU consumption pulled from `/proc/<daemon-pid>/stat`
  (utime + stime + cutime + cstime). Resolution is 1 USER_HZ tick
  (10 ms on a default Linux), so the long runs sample 300 s at a
  time to keep the signal above the noise floor.

* **Wall-clock metrics** (PSI reaction latency, real compression
  syscall time) come from log line timestamps emitted by the daemon
  in compact format.

## Quick start

```bash
# Build first (you need clang + bpftool installed — see project README)
cd ../daemon && cargo build --release

# Then from the bench/ directory:
./scripts/bench-cpu.sh             # ~15 min — three 300s sampling windows
./scripts/bench-psi-latency.sh     # ~2 min — induces 14 GiB allocation
./scripts/bench-real-compress.sh   # ~1 min — one-shot on largest target
./scripts/bench-recompression.sh   # ~2 min — aggressive idle thresholds
```

Each script writes a timestamped result file under `results/` and
prints a plain-text summary on stdout.

## Caveats

* **Capability prompts**: scripts call `sudo setcap` and `sudo cp` —
  they will prompt for the password. Read each before running if you
  prefer to grant manually.

* **Live workload variance**: results vary 10–30% across runs because
  the underlying browser activity is not controlled. Run each test
  several times if you need a tight confidence interval.

* **Memory pressure test (B)** allocates 14 GiB in a child Python
  process. On a 16 GiB box without swap this can trigger the OOM
  killer — adjust the alloc size in the script if your box is smaller.

* **Real compression test (C)** actually pages out memory. Run it on
  a tab you don't mind being briefly paused on next access.
