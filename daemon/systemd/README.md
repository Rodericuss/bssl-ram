# systemd integration

Run `bssl-ram` as a **system template service** that drops to your user UID
and keeps `CAP_SYS_NICE` + `CAP_SYS_PTRACE` as ambient capabilities — no
permanent root, no `sudo` after install.

## Why a system template service (and not `--user`)

`process_madvise(2)` and reading `/proc/PID/smaps_rollup` both go through
`ptrace_may_access()`. On Arch with the default `kernel.yama.ptrace_scope=1`
those checks succeed only if the caller has `CAP_SYS_PTRACE` **in the same
user namespace as the target**.

`systemd --user` services run inside their own user namespace, so any
ambient cap granted there cannot satisfy a ptrace check against Firefox
running in the init userns. A system service can drop privileges with
`User=` while staying in the init userns, which keeps the caps usable.

The unit is a template (`bssl-ram@.service`), instantiated per user:

```
sudo systemctl start bssl-ram@gabrielmaia.service
```

## Install

```bash
# 1. Install the binary
cd daemon
cargo build --release
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram

# 2. Install the template unit
sudo install -Dm644 systemd/bssl-ram@.service /etc/systemd/system/bssl-ram@.service
sudo systemctl daemon-reload

# 3. Enable for your user (replace with your actual login)
sudo systemctl enable --now bssl-ram@$USER.service
```

## Verify

```bash
systemctl status bssl-ram@$USER.service
journalctl -u bssl-ram@$USER -f

# Check the runtime caps
PID=$(systemctl show bssl-ram@$USER.service -p MainPID --value)
capsh --decode=$(awk '/^CapEff:/ {print $2}' /proc/$PID/status)
# expect: cap_sys_ptrace,cap_sys_nice
```

After ~30s of any tab being idle you should see compression in the journal:

```
INFO compressing pid 222139 (RSS: 85 MiB)
INFO pid 222139 paged out 50 MiB to zram in 1 batch(es) (0 MiB skipped by kernel)
```

## Sandbox notes

The unit applies a moderate sandbox (`ProtectSystem=strict`,
`ProtectHome=read-only`, `PrivateNetwork`, `MemoryMax=128M`, etc.). It
deliberately does **not** enable the heavy options (`PrivateUsers`,
`ProtectKernelTunables`, `RestrictNamespaces`, `SystemCallFilter`) because
those interfere with the very `ptrace_may_access()` checks the daemon
needs to make.

Audit the resulting posture with:

```bash
systemd-analyze security bssl-ram@$USER
```

## Alternative: file capabilities (no systemd)

If you prefer to skip systemd entirely:

```bash
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram
sudo setcap cap_sys_nice,cap_sys_ptrace+eip /usr/local/bin/bssl-ram
/usr/local/bin/bssl-ram
```

The capabilities are baked into the binary's xattrs, so it doesn't need
`sudo` at runtime. You give up auto-restart and journald integration, but
it works the same.
