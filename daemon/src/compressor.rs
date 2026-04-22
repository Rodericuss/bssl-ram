use anyhow::{Context, Result};
use libc::{c_int, c_long, iovec};
use std::fs;
use tracing::{debug, info, warn};

// process_madvise syscall number on x86_64 Linux
// https://elixir.bootlin.com/linux/latest/source/arch/x86/entry/syscalls/syscall_64.tbl
const SYS_PROCESS_MADVISE: c_long = 440;
const MADV_PAGEOUT: c_int = 21;

/// Opens a pidfd for the given PID.
/// pidfd is the modern Linux API for safely referencing processes.
fn open_pidfd(pid: u32) -> Result<c_int> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        anyhow::bail!("pidfd_open failed for pid {}: {}", pid, std::io::Error::last_os_error());
    }
    Ok(fd as c_int)
}

/// Reads anonymous memory regions from /proc/PID/smaps.
/// Only anonymous regions can be paged out via MADV_PAGEOUT.
/// Shared and file-backed regions are skipped.
fn read_anonymous_regions(pid: u32) -> Result<Vec<(usize, usize)>> {
    let smaps = fs::read_to_string(format!("/proc/{}/smaps", pid))
        .with_context(|| format!("reading smaps for pid {}", pid))?;

    let mut regions = Vec::new();
    let mut current_start = 0usize;
    let mut current_size = 0usize;
    let mut in_anon = false;

    for line in smaps.lines() {
        // Address range line: "7f3a00000000-7f3a10000000 rw-p 00000000 00:00 0"
        if line.contains('-') && line.contains(' ') {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                let range: Vec<&str> = parts[0].split('-').collect();
                if range.len() == 2 {
                    if let (Ok(start), Ok(end)) =
                        (usize::from_str_radix(range[0], 16),
                         usize::from_str_radix(range[1], 16))
                    {
                        // 'p' = private mapping (anonymous candidate)
                        // shared mappings have 's' and cannot be paged out
                        in_anon = parts.get(1).map_or(false, |f| f.contains('p'))
                            && parts[4] == "0"; // inode == 0 means anonymous
                        current_start = start;
                        current_size = end - start;
                    }
                }
            }
        }

        // Anonymous field confirms there's actual anonymous memory here
        if in_anon && line.starts_with("Anonymous:") {
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

/// Returns RSS in MiB for a process.
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

/// Calls process_madvise(MADV_PAGEOUT) on all anonymous regions of a process.
/// This tells the kernel to move those pages to swap (zram) immediately.
/// The process is never paused or signalled — it continues running.
pub fn compress_pid(pid: u32, dry_run: bool) -> Result<()> {
    let rss = rss_mib(pid);
    info!("compressing pid {} (RSS: {} MiB)", pid, rss);

    let regions = read_anonymous_regions(pid)
        .with_context(|| format!("reading regions for pid {}", pid))?;

    debug!("pid {} has {} anonymous regions", pid, regions.len());

    if dry_run {
        info!("[dry-run] would page out {} regions for pid {}", regions.len(), pid);
        return Ok(());
    }

    let pidfd = open_pidfd(pid)?;

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
                1usize,       // iov count
                MADV_PAGEOUT,
                0u32,         // flags (must be 0)
            )
        };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            // EPERM is expected for some regions (kernel-managed, locked pages)
            // We warn but don't abort — partial compression is still useful
            warn!("MADV_PAGEOUT failed for region 0x{:x} size {}: {}", start, size, err);
        }
    }

    unsafe { libc::close(pidfd) };

    info!("pid {} paged out successfully", pid);
    Ok(())
}
