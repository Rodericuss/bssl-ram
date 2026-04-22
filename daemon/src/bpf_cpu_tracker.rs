//! eBPF-based per-task CPU runtime tracker.
//!
//! Loads a BPF program at startup that hooks `raw_tp/sched_switch` and
//! accumulates per-PID CPU nanoseconds in a kernel hash map. Userspace
//! reads the map via [`runtime_ns`] each scan cycle instead of opening
//! and parsing `/proc/PID/stat` for every target — moving the discovery
//! work from per-cycle file I/O to in-kernel tracepoint accounting.
//!
//! The BPF object is compiled at build time (see `build.rs`) and
//! embedded into the binary; no external `.o` ships at runtime.
//!
//! Permissions:
//!   - Loading the program needs CAP_BPF (kernel ≥ 5.8) and effectively
//!     CAP_PERFMON for the `bpf_ktime_get_ns` helper. The systemd unit
//!     grants both. Where the load fails (older kernel, missing caps,
//!     verifier rejection), the daemon falls back to /proc/PID/stat
//!     polling — the rest of the pipeline is unchanged.

use anyhow::{Context, Result};
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::MapCore;
use std::mem::MaybeUninit;
use tracing::{debug, info};

mod skel {
    include!(concat!(env!("OUT_DIR"), "/cpu_tracker.skel.rs"));
}

use skel::{CpuTrackerSkel, CpuTrackerSkelBuilder};

/// Owns the loaded + attached BPF skeleton. Holding this alive keeps
/// the kernel programs and maps live; dropping it detaches everything.
pub struct BpfCpuTracker {
    // The skeleton borrows from the OpenObject storage; we Box the
    // storage so the borrow stays valid while the field structure
    // moves around.
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: CpuTrackerSkel<'static>,
}

impl BpfCpuTracker {
    /// Load the BPF object, run the verifier, attach to sched_switch.
    /// Fails with a context-rich error so the caller can decide whether
    /// to log and degrade or to abort.
    pub fn load() -> Result<Self> {
        let builder = CpuTrackerSkelBuilder::default();

        // The skeleton borrows from this storage — boxing keeps it
        // pinned at a stable address while we move the BpfCpuTracker
        // around (e.g. into an Option in main).
        let mut storage = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let storage_ref: &'static mut MaybeUninit<libbpf_rs::OpenObject> = unsafe {
            // SAFETY: the storage is owned by self._open_object for the
            // entire lifetime of the skeleton inside self.skel; the
            // 'static lift is sound because we never expose it past
            // this struct's drop.
            std::mem::transmute(storage.as_mut())
        };

        let open = builder
            .open(storage_ref)
            .context("opening BPF skeleton (libbpf_rs::ObjectBuilder::open)")?;
        let mut loaded = open
            .load()
            .context("loading BPF object — verifier rejection or missing CAP_BPF?")?;
        loaded
            .attach()
            .context("attaching raw_tp/sched_switch — kernel without CONFIG_BPF_EVENTS?")?;

        info!("eBPF CPU tracker loaded — sched_switch hook live");

        Ok(Self {
            _open_object: storage,
            skel: loaded,
        })
    }

    /// Read the cumulative on-CPU nanoseconds for `pid` (must be the
    /// thread-group leader / process tgid). Returns `None` when the
    /// kernel hasn't seen the task on a CPU yet — in that case the
    /// caller should treat it as a first observation and skip CPU
    /// delta calculation, exactly like the /proc path.
    pub fn runtime_ns(&self, pid: u32) -> Option<u64> {
        let key = pid.to_ne_bytes();
        let bytes = self
            .skel
            .maps
            .task_runtime
            .lookup(&key, libbpf_rs::MapFlags::ANY)
            .ok()
            .flatten()?;
        let arr: [u8; 8] = bytes.as_slice().try_into().ok()?;
        Some(u64::from_ne_bytes(arr))
    }

    /// Drop tracking entries for a PID that exited. Best-effort —
    /// stale entries are harmless (worst case the same PID gets reused
    /// later and inherits a high counter, but the tracker's starttime
    /// guard catches that).
    #[allow(dead_code)]
    pub fn forget(&self, pid: u32) {
        let key = pid.to_ne_bytes();
        let _ = self.skel.maps.task_runtime.delete(&key);
        let _ = self.skel.maps.task_start.delete(&key);
        debug!(pid, "bpf cpu tracker: forget");
    }
}
