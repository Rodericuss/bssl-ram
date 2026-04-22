use anyhow::{Context, Result};
use libc::{c_int, c_long, iovec};
use std::fs;
use tracing::{debug, info, warn};

// process_madvise syscall number on x86_64 Linux (kernel >= 5.10)
const SYS_PROCESS_MADVISE: c_long = 440;
const MADV_PAGEOUT: c_int = 21;

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

/// Reads anonymous private memory regions from /proc/PID/smaps.
/// Only these regions can be safely paged out via MADV_PAGEOUT:
///   - private ('p' flag, not shared 's')
///   - anonymous (inode == 0, not file-backed)
///   - have actual anonymous pages (Anonymous: > 0 kB)
fn read_anonymous_regions(pid: u32) -> Result<Vec<(usize, usize)>> {
    let smaps = fs::read_to_string(format!("/proc/{}/smaps", pid))
        .with_context(|| format!("reading smaps for pid {}", pid))?;

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

        // Confirm there are actual anonymous pages in this region
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

    Ok(regions)
}

/// Returns RSS in MiB for a process by reading /proc/PID/smaps_rollup.
pub fn rss_mib(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{}/smaps_rollup", pid))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Rss:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
        / 1024
}

/// Reads CPU ticks (utime + stime) from /proc/PID/stat.
/// Returns None if the process no longer exists.
pub fn read_cpu_ticks(pid: u32) -> Option<(u64, u64)> {
    let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;

    // /proc/PID/stat format:
    // pid (comm) state ppid ... utime(14) stime(15) ...
    // The comm field can contain spaces and parentheses, so we find
    // the last ')' and count fields from there.
    let after_comm = stat.rfind(')')?;
    let fields: Vec<&str> = stat[after_comm + 2..].split_whitespace().collect();

    // After the closing paren: state(0) ppid(1) pgrp(2) session(3)
    // tty_nr(4) tpgid(5) flags(6) minflt(7) cminflt(8) majflt(9)
    // cmajflt(10) utime(11) stime(12)
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;

    Some((utime, stime))
}

/// Calls process_madvise(MADV_PAGEOUT) on all anonymous regions of a process.
/// This instructs the kernel to move those pages to swap (zram) immediately.
///
/// The process is never paused, signalled, or modified in any way.
/// When it next accesses a paged-out address, the kernel transparently
/// decompresses and restores the page — the process never notices.
pub fn compress_pid(pid: u32, dry_run: bool) -> Result<()> {
    let rss = rss_mib(pid);
    info!("compressing pid {} (RSS: {} MiB)", pid, rss);

    let regions = read_anonymous_regions(pid)
        .with_context(|| format!("reading regions for pid {}", pid))?;

    debug!("pid {} has {} anonymous regions", pid, regions.len());

    if dry_run {
        info!(
            "[dry-run] would page out {} regions for pid {} ({} MiB)",
            regions.len(),
            pid,
            rss
        );
        return Ok(());
    }

    let pidfd = open_pidfd(pid)?;

    let mut paged_out = 0usize;
    for (start, size) in &regions {
        let iov = iovec {
            iov_base: *start as *mut libc::c_void,
            iov_len: *size,
        };

        let ret = unsafe {
            libc::syscall(
                SYS_PROCESS_MADVISE,
                pidfd,
                &iov as *const iovec,
                1usize,       // iovec count
                MADV_PAGEOUT,
                0u32,         // flags (must be 0)
            )
        };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            // EPERM is expected for some regions (locked pages, vDSO, etc.)
            // EINVAL for huge pages or special mappings
            // We warn but continue — partial compression is still useful
            warn!(
                "MADV_PAGEOUT failed for pid {} region 0x{:x}+{}: {}",
                pid, start, size, err
            );
        } else {
            paged_out += size;
        }
    }

    unsafe { libc::close(pidfd) };

    info!(
        "pid {} paged out ~{} MiB to zram",
        pid,
        paged_out / 1024 / 1024
    );
    Ok(())
}
