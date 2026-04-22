// Compares old (1 syscall per iovec) vs new (batched up to IOV_MAX) cost
// of process_madvise(MADV_PAGEOUT) on the same target process.
//
// Usage: sudo ./bench_batching <pid>
//
// We do NOT actually page memory out — we issue MADV_COLD instead, which
// only marks pages cold without triggering swap I/O. Same syscall path,
// same iovec walk, same TLB shootdown — but no zram churn between runs,
// so the timing comparison is honest.

#[path = "../src/compressor.rs"]
mod compressor;

use libc::{c_int, c_long, iovec};
use std::fs;
use std::time::Instant;

const SYS_PROCESS_MADVISE: c_long = 440;
const SYS_PIDFD_OPEN: c_long = libc::SYS_pidfd_open;
const MADV_COLD: c_int = 20;
const IOV_MAX: usize = 1024;

fn open_pidfd(pid: u32) -> c_int {
    let fd = unsafe { libc::syscall(SYS_PIDFD_OPEN, pid, 0) };
    assert!(fd >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
    fd as c_int
}

fn anon_regions(pid: u32) -> Vec<(usize, usize)> {
    let smaps = fs::read_to_string(format!("/proc/{}/smaps", pid)).expect("read smaps");
    let mut regions = Vec::new();
    let mut current_start = 0usize;
    let mut current_size = 0usize;
    let mut is_private_anon = false;

    for line in smaps.lines() {
        if let Some(dash) = line.find('-') {
            let rest = &line[dash + 1..];
            if let Some(sp) = rest.find(' ') {
                let end_str = &rest[..sp];
                let after = &rest[sp + 1..];
                let parts: Vec<&str> = after.split_whitespace().collect();
                if parts.len() >= 4 {
                    if let (Ok(start), Ok(end)) = (
                        usize::from_str_radix(&line[..dash], 16),
                        usize::from_str_radix(end_str, 16),
                    ) {
                        is_private_anon = parts[0].contains('p') && parts[3] == "0";
                        current_start = start;
                        current_size = end - start;
                    }
                }
            }
        }
        if is_private_anon && line.starts_with("Anonymous:") {
            let kb: usize = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if kb > 0 {
                regions.push((current_start, current_size));
            }
        }
    }
    regions
}

fn one_per_call(pidfd: c_int, iovs: &[iovec]) -> (usize, std::time::Duration) {
    let started = Instant::now();
    let mut bytes = 0usize;
    for iov in iovs {
        let ret = unsafe {
            libc::syscall(
                SYS_PROCESS_MADVISE,
                pidfd,
                iov as *const iovec,
                1usize,
                MADV_COLD,
                0u32,
            )
        };
        if ret > 0 {
            bytes += ret as usize;
        }
    }
    (bytes, started.elapsed())
}

fn batched(pidfd: c_int, iovs: &[iovec]) -> (usize, std::time::Duration, usize) {
    let started = Instant::now();
    let mut bytes = 0usize;
    let mut chunks = 0usize;
    for chunk in iovs.chunks(IOV_MAX) {
        let ret = unsafe {
            libc::syscall(
                SYS_PROCESS_MADVISE,
                pidfd,
                chunk.as_ptr(),
                chunk.len(),
                MADV_COLD,
                0u32,
            )
        };
        if ret > 0 {
            bytes += ret as usize;
        }
        chunks += 1;
    }
    (bytes, started.elapsed(), chunks)
}

fn main() {
    let pid: u32 = std::env::args()
        .nth(1)
        .expect("usage: bench_batching <pid>")
        .parse()
        .expect("pid must be u32");

    println!("target pid: {}", pid);
    let regions = anon_regions(pid);
    let iovs: Vec<iovec> = regions
        .iter()
        .map(|(s, n)| iovec {
            iov_base: *s as *mut libc::c_void,
            iov_len: *n,
        })
        .collect();
    println!("{} private-anon regions ({} chunks of {})", iovs.len(), iovs.len().div_ceil(IOV_MAX), IOV_MAX);

    let pidfd = open_pidfd(pid);

    // Warm-up — first call into kernel/cache may be skewed
    let _ = batched(pidfd, &iovs);

    let (b1, t1) = one_per_call(pidfd, &iovs);
    let (b2, t2, chunks) = batched(pidfd, &iovs);

    println!("\n=== results (MADV_COLD, no actual swap I/O) ===");
    println!("one-per-call : {:>4} syscalls, advised {:>10} bytes, took {:?}", iovs.len(), b1, t1);
    println!("batched      : {:>4} syscalls, advised {:>10} bytes, took {:?}", chunks, b2, t2);

    let speedup = t1.as_nanos() as f64 / t2.as_nanos().max(1) as f64;
    let syscall_reduction = iovs.len() as f64 / chunks.max(1) as f64;
    println!("\nspeedup       : {:.1}x", speedup);
    println!("syscall reduc.: {:.1}x", syscall_reduction);

    unsafe { libc::close(pidfd) };

    // Force compressor symbol use so the dead-code lint stays quiet
    let _ = compressor::rss_mib;
}
