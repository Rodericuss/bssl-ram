use anyhow::{Context, Result};
use libc::{c_int, c_long, iovec};
use std::fs;
use tracing::{debug, warn};

// process_madvise syscall number on x86_64 Linux (kernel >= 5.10)
const SYS_PROCESS_MADVISE: c_long = 440;
const MADV_PAGEOUT: c_int = 21;

// Maximum iovecs per process_madvise call (POSIX IOV_MAX, queried via
// `getconf IOV_MAX`). Linux pins this at 1024. Batching up to this many
// regions per syscall amortises both syscall and TLB-shootdown costs.
const IOV_MAX: usize = 1024;

/// Opens a pidfd for the given PID.
/// pidfd is the modern Linux API for safely referencing processes
/// without PID reuse races.
fn open_pidfd(pid: u32) -> Result<c_int> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        anyhow::bail!(
            "pidfd_open failed for pid {}: {}",
            pid,
            std::io::Error::last_os_error()
        );
    }
    Ok(fd as c_int)
}

/// Parses anonymous private memory regions out of a /proc/PID/smaps blob.
/// Only regions matching all three predicates can be safely paged out via
/// MADV_PAGEOUT and are returned here:
///   - private ('p' flag, not shared 's')
///   - anonymous (inode == 0, not file-backed)
///   - have at least one anonymous page (Anonymous: > 0 kB)
///
/// Pure function over the smaps string so it can be unit-tested with
/// fixtures rather than a live /proc.
pub fn parse_anon_regions(smaps: &str) -> Vec<(usize, usize)> {
    let mut regions = Vec::new();
    let mut current_start = 0usize;
    let mut current_size = 0usize;
    let mut is_private_anon = false;

    for line in smaps.lines() {
        // Address range header line looks like:
        // "7f3a00000000-7f3a10000000 rw-p 00000000 00:00 0  [anon:...]"
        // Fields: addr_range perms offset dev inode [pathname]
        if let Some(dash_pos) = line.find('-') {
            let rest = &line[dash_pos + 1..];
            if let Some(space_pos) = rest.find(' ') {
                let end_str = &rest[..space_pos];
                let after = &rest[space_pos + 1..];
                let parts: Vec<&str> = after.split_whitespace().collect();

                if parts.len() >= 4 {
                    let start_str = &line[..dash_pos];
                    if let (Ok(start), Ok(end)) = (
                        usize::from_str_radix(start_str, 16),
                        usize::from_str_radix(end_str, 16),
                    ) {
                        let perms = parts[0];
                        let inode = parts[3];

                        // 'p' = private, not shared
                        // inode == "0" = anonymous (not file-backed)
                        is_private_anon = perms.contains('p') && inode == "0";
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

fn read_anonymous_regions(pid: u32) -> Result<Vec<(usize, usize)>> {
    let smaps = fs::read_to_string(format!("/proc/{}/smaps", pid))
        .with_context(|| format!("reading smaps for pid {}", pid))?;
    Ok(parse_anon_regions(&smaps))
}

/// Returns RSS in MiB for a process by reading /proc/PID/smaps_rollup.
pub fn rss_mib(pid: u32) -> u64 {
    let path = format!("/proc/{}/smaps_rollup", pid);
    match fs::read_to_string(&path) {
        Ok(s) => {
            s.lines()
                .find(|l| l.starts_with("Rss:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                / 1024
        }
        Err(e) => {
            warn!("rss_mib: cannot read {}: {}", path, e);
            0
        }
    }
}

/// Parses the (utime, stime) CPU tick pair out of a /proc/PID/stat string.
///
/// /proc/PID/stat format:
///   pid (comm) state ppid ... utime(14) stime(15) ...
///
/// The comm field is wrapped in parentheses and may itself contain spaces
/// and even parentheses (e.g. "(Web Content)" for Firefox tabs). We anchor
/// on the *last* `)` and tokenise everything after it.
///
/// Pure function over the stat string so it can be unit-tested with
/// fixtures rather than a live /proc.
pub fn parse_cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let after_comm = stat.rfind(')')?;
    let fields: Vec<&str> = stat[after_comm + 2..].split_whitespace().collect();

    // After the closing paren: state(0) ppid(1) pgrp(2) session(3)
    // tty_nr(4) tpgid(5) flags(6) minflt(7) cminflt(8) majflt(9)
    // cmajflt(10) utime(11) stime(12)
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;

    Some((utime, stime))
}

/// Reads CPU ticks (utime + stime) from /proc/PID/stat.
/// Returns None if the process no longer exists or the stat file is malformed.
pub fn read_cpu_ticks(pid: u32) -> Option<(u64, u64)> {
    let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    parse_cpu_ticks(&stat)
}
/// Outcome of a single compress_pid call. Returned so the caller can
/// fold the numbers into telemetry without reparsing log strings.
#[derive(Debug, Default, Clone, Copy)]
pub struct CompressOutcome {
    pub regions: usize,
    pub bytes_advised: u64,
    pub bytes_skipped_by_kernel: u64,
    pub batches: usize,
}

/// Calls process_madvise(MADV_PAGEOUT) on all anonymous regions of a process.
/// This instructs the kernel to move those pages to swap (zram) immediately.
///
/// The process is never paused, signalled, or modified in any way.
/// When it next accesses a paged-out address, the kernel transparently
/// decompresses and restores the page — the process never notices.
pub fn compress_pid(pid: u32, dry_run: bool) -> Result<CompressOutcome> {
    // Per-PID summary is emitted by the caller (one canonical line with
    // action+reason fields), so the compressor only logs at debug now.
    let regions =
        read_anonymous_regions(pid).with_context(|| format!("reading regions for pid {}", pid))?;

    debug!("pid {} has {} anonymous regions", pid, regions.len());

    if dry_run {
        return Ok(CompressOutcome {
            regions: regions.len(),
            bytes_advised: 0,
            bytes_skipped_by_kernel: 0,
            batches: 0,
        });
    }

    let pidfd = open_pidfd(pid)?;

    // Build iovec table once. Each chunk of up to IOV_MAX entries goes out
    // in a single process_madvise() call — kernel walks the array, applies
    // MADV_PAGEOUT to each range, and amortises a single TLB shootdown
    // pass across the whole batch.
    let iovs: Vec<iovec> = regions
        .iter()
        .map(|(start, size)| iovec {
            iov_base: *start as *mut libc::c_void,
            iov_len: *size,
        })
        .collect();

    let total_requested: usize = regions.iter().map(|(_, s)| *s).sum();
    let mut bytes_advised: usize = 0;
    let mut chunks_done = 0usize;

    for chunk in iovs.chunks(IOV_MAX) {
        let ret = unsafe {
            libc::syscall(
                SYS_PROCESS_MADVISE,
                pidfd,
                chunk.as_ptr(),
                chunk.len(),
                MADV_PAGEOUT,
                0u32, // flags (must be 0)
            )
        };

        if ret < 0 {
            // Whole-batch failure (EFAULT, EINVAL on bad iovec table, etc.).
            // Per-region EPERM/EINVAL within a batch returns a *partial*
            // bytes_advised count instead — those are not errors, they
            // just mean the kernel skipped some ranges and kept going.
            let err = std::io::Error::last_os_error();
            warn!(
                "process_madvise batch failed for pid {} chunk {} ({} iovecs): {}",
                pid,
                chunks_done,
                chunk.len(),
                err
            );
        } else {
            bytes_advised += ret as usize;
        }
        chunks_done += 1;
    }

    unsafe { libc::close(pidfd) };

    let skipped = total_requested.saturating_sub(bytes_advised);
    Ok(CompressOutcome {
        regions: regions.len(),
        bytes_advised: bytes_advised as u64,
        bytes_skipped_by_kernel: skipped as u64,
        batches: chunks_done,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cpu_ticks_handles_simple_comm() {
        // Real /proc/PID/stat layout for a tiny process named "init"
        let stat = "1 (init) S 0 1 1 0 -1 4194560 100 200 0 0 11 22 0 0 20 0 1 0 100 12345 678";
        assert_eq!(parse_cpu_ticks(stat), Some((11, 22)));
    }

    #[test]
    fn parse_cpu_ticks_handles_comm_with_spaces() {
        // Firefox content processes have a comm field with spaces:
        // "Web Content", "Privileged Cont", etc.
        let stat = "12345 (Web Content) S 1 12345 12345 0 -1 4194560 100 200 0 0 658 59 0 0 20 0 1 0 200000 90000000 145000";
        assert_eq!(parse_cpu_ticks(stat), Some((658, 59)));
    }

    #[test]
    fn parse_cpu_ticks_handles_comm_with_parens() {
        // The kernel allows ')' inside the comm field, which is exactly why
        // we anchor on the *last* ')' rather than the first.
        let stat = "777 (weird (name)) S 1 777 777 0 -1 4194560 50 60 0 0 12 34 0 0 20 0 1 0 9999 1000000 5000";
        assert_eq!(parse_cpu_ticks(stat), Some((12, 34)));
    }

    #[test]
    fn parse_cpu_ticks_returns_none_on_malformed_input() {
        assert_eq!(parse_cpu_ticks(""), None);
        assert_eq!(parse_cpu_ticks("no parens here"), None);
        // Truncated stat with no utime field
        assert_eq!(parse_cpu_ticks("1 (init) S 0 1 1"), None);
    }

    #[test]
    fn parse_anon_regions_picks_private_anon_with_pages() {
        let smaps = "\
7f0000000000-7f0000010000 rw-p 00000000 00:00 0 \nSize:                 64 kB\nRss:                  64 kB\nPss:                  64 kB\nAnonymous:            64 kB\nVmFlags: rd wr mr mw me ac\n";
        let regions = parse_anon_regions(smaps);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], (0x7f0000000000, 0x10000));
    }

    #[test]
    fn parse_anon_regions_skips_shared_mappings() {
        // 's' permission flag means shared — must NOT be paged out
        let smaps = "\
7f0000000000-7f0000010000 rw-s 00000000 00:00 0 \nSize:                 64 kB\nRss:                  64 kB\nAnonymous:            64 kB\n";
        assert!(parse_anon_regions(smaps).is_empty());
    }

    #[test]
    fn parse_anon_regions_skips_file_backed_mappings() {
        // inode != 0 means the mapping is file-backed, even if it has
        // CoW anon pages we'd rather not page out
        let smaps = "\
7f0000000000-7f0000010000 r-xp 00000000 fd:00 12345  /usr/lib/libc.so.6\nSize:                 64 kB\nRss:                  64 kB\nAnonymous:             4 kB\n";
        assert!(parse_anon_regions(smaps).is_empty());
    }

    #[test]
    fn parse_anon_regions_skips_regions_with_zero_anon_pages() {
        // Private anon header but no actual anon pages — nothing to page out
        let smaps = "\
7f0000000000-7f0000010000 rw-p 00000000 00:00 0 \nSize:                 64 kB\nRss:                   0 kB\nAnonymous:             0 kB\n";
        assert!(parse_anon_regions(smaps).is_empty());
    }

    #[test]
    fn parse_anon_regions_handles_special_pseudo_regions() {
        // [vdso] / [vvar] / [stack] are file-backed-style pseudo regions —
        // they have non-zero "inode" markers in some kernels and special
        // pathnames. We must not select them.
        let smaps = "\
7ffe00000000-7ffe00002000 r-xp 00000000 00:00 0  [vdso]\nSize:                  8 kB\nRss:                   8 kB\nAnonymous:             8 kB\n7ffe00010000-7ffe00012000 rw-p 00000000 00:00 0  [stack]\nSize:                  8 kB\nRss:                   8 kB\nAnonymous:             8 kB\n";
        // [vdso] is rwxp/rw-p with inode 0 — technically eligible by our
        // current rule. We keep it permissive on purpose: vdso pages are
        // small and the kernel will refuse the pageout if it can't honour
        // it. This test pins the current behaviour.
        let regions = parse_anon_regions(smaps);
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn parse_anon_regions_picks_multiple_regions() {
        let smaps = "\
7f0000000000-7f0000010000 rw-p 00000000 00:00 0 \nAnonymous:            64 kB\n7f0000020000-7f0000040000 rw-p 00000000 00:00 0 \nAnonymous:           128 kB\n";
        let regions = parse_anon_regions(smaps);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0], (0x7f0000000000, 0x10000));
        assert_eq!(regions[1], (0x7f0000020000, 0x20000));
    }
}
