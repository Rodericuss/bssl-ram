// SPDX-License-Identifier: GPL-2.0
//
// cpu_tracker — per-task CPU runtime accumulator via raw_tp/sched_switch.
//
// Hooks the kernel scheduler's switch tracepoint and accumulates CPU
// nanoseconds per (thread-group leader) PID inside a hash map.
// Userspace reads the map at scan-cycle time instead of opening
// /proc/PID/stat for every target — the per-cycle work shrinks from
// O(targets) syscalls to O(1) map lookups (one per target, but no
// /proc walks, no parsing, no kernel-user data copy beyond the u64).
//
// The trick is that the kernel already does this accounting internally;
// we just expose it without forcing userspace to ask /proc each cycle.

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "GPL";

// ---------------------------------------------------------------
// Maps
//
// task_runtime: tgid → cumulative CPU nanoseconds since the BPF
//               program was loaded. Userspace reads this; deltas
//               between successive reads give per-cycle CPU usage,
//               which is what the daemon needs for idle detection.
//
// task_start:   tgid → timestamp (bpf_ktime_get_ns) at which the
//               task was last scheduled in. Used to compute the
//               increment to add to task_runtime when the task
//               gets scheduled out.
// ---------------------------------------------------------------

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 16384);
    __type(key, __u32);
    __type(value, __u64);
} task_runtime SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 16384);
    __type(key, __u32);
    __type(value, __u64);
} task_start SEC(".maps");

// task_birth: tgid → task_struct->start_time captured the first time
// we observe this tgid being scheduled. Used as the per-task identity
// nonce in the daemon's CpuTracker (PID reuse → different start_time
// → state wiped). One write per task lifetime; reads are cheap.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 16384);
    __type(key, __u32);
    __type(value, __u64);
} task_birth SEC(".maps");

// ---------------------------------------------------------------
// raw_tp/sched_switch fires on every context switch with three args:
//   ctx->args[0]  bool         preempt
//   ctx->args[1]  task_struct* prev (going off CPU)
//   ctx->args[2]  task_struct* next (going on CPU)
//
// We accumulate runtime for prev (now - start) and reset start for
// next. Per-thread events are squashed onto the thread-group leader
// pid (tgid) so userspace sees process-level totals.
// ---------------------------------------------------------------

SEC("raw_tp/sched_switch")
int handle_sched_switch(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *prev = (struct task_struct *)ctx->args[1];
    struct task_struct *next = (struct task_struct *)ctx->args[2];

    __u64 now = bpf_ktime_get_ns();

    __u32 prev_tgid = BPF_CORE_READ(prev, tgid);
    __u32 next_tgid = BPF_CORE_READ(next, tgid);

    // Account elapsed runtime to the task that just left the CPU.
    if (prev_tgid != 0) {
        __u64 *start = bpf_map_lookup_elem(&task_start, &prev_tgid);
        if (start) {
            __u64 delta = now - *start;
            __u64 *runtime = bpf_map_lookup_elem(&task_runtime, &prev_tgid);
            if (runtime) {
                __sync_fetch_and_add(runtime, delta);
            } else {
                bpf_map_update_elem(&task_runtime, &prev_tgid, &delta, BPF_ANY);
            }
        }
    }

    // Mark the task that just got the CPU.
    if (next_tgid != 0) {
        bpf_map_update_elem(&task_start, &next_tgid, &now, BPF_ANY);

        // Capture birth (start_time) once per task. start_time on
        // modern kernels is nanoseconds-since-boot; per-task unique +
        // monotonic, exactly what userspace needs as a PID-reuse nonce.
        __u64 *seen = bpf_map_lookup_elem(&task_birth, &next_tgid);
        if (!seen) {
            __u64 birth = BPF_CORE_READ(next, start_time);
            bpf_map_update_elem(&task_birth, &next_tgid, &birth, BPF_NOEXIST);
        }
    }

    return 0;
}
