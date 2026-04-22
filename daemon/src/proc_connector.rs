//! cn_proc — process events via netlink connector.
//!
//! The kernel publishes per-task lifecycle events (fork/exec/exit/...)
//! over an `AF_NETLINK` / `NETLINK_CONNECTOR` socket bound to the
//! `CN_IDX_PROC` multicast group. By subscribing once we get a stream
//! of every process change in the system; the daemon can then maintain
//! its own in-memory PID table instead of walking `/proc/[0-9]+` every
//! scan cycle.
//!
//! Pure libc, zero extra dependencies — repr(C) structs mirror the
//! kernel's `linux/connector.h` and `linux/cn_proc.h`.
//!
//! References:
//!   - <https://lwn.net/Articles/427351/>
//!   - <https://www.kernel.org/doc/Documentation/connector/connector.rst>
//!   - include/uapi/linux/cn_proc.h
//!
//! Permissions: subscribing to PROC_CN_MCAST_LISTEN historically required
//! CAP_NET_ADMIN. Recent kernels allow unprivileged use; the systemd
//! unit grants the cap regardless so the feature works everywhere. If
//! the bind/send fails the daemon logs once and falls back to walking
//! /proc per cycle (see main.rs).

use anyhow::{bail, Result};
use libc::{
    bind, c_int, c_void, recv, send, setsockopt, sockaddr_nl, socket, AF_NETLINK, MSG_DONTWAIT,
    SOCK_CLOEXEC, SOCK_DGRAM, SOL_SOCKET, SO_RCVBUF,
};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

const NETLINK_CONNECTOR: c_int = 11;

/// Multicast group identifier for the proc connector. Hard-coded in
/// the kernel (`include/uapi/linux/connector.h`).
const CN_IDX_PROC: u32 = 0x1;
const CN_VAL_PROC: u32 = 0x1;

/// Operation codes the listener can send to the kernel.
const PROC_CN_MCAST_LISTEN: u32 = 1;
#[allow(dead_code)]
const PROC_CN_MCAST_IGNORE: u32 = 2;

/// Subset of `enum proc_event::what` we actually act on. Other event
/// types (UID, GID, SID, PTRACE, COMM, COREDUMP) are ignored at parse
/// time — we just don't translate them into `ProcEvent` variants.
const PROC_EVENT_FORK: u32 = 0x0000_0001;
const PROC_EVENT_EXEC: u32 = 0x0000_0002;
const PROC_EVENT_EXIT: u32 = 0x8000_0000;

/// `struct nlmsghdr` from `linux/netlink.h`. We only need the size and
/// don't read it back — `recv()` returns a buffer that starts with this
/// header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Nlmsghdr {
    nlmsg_len: u32,
    nlmsg_type: u16,
    nlmsg_flags: u16,
    nlmsg_seq: u32,
    nlmsg_pid: u32,
}

/// `struct cb_id` from `linux/connector.h`. Identifies the connector
/// channel inside netlink.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CbId {
    idx: u32,
    val: u32,
}

/// `struct cn_msg` from `linux/connector.h`. Wraps the proc_event
/// payload inside the netlink message.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CnMsg {
    id: CbId,
    seq: u32,
    ack: u32,
    len: u16,
    flags: u16,
}

/// `struct proc_event` header from `linux/cn_proc.h`. The kernel
/// unions the event-specific data after this; we manually parse the
/// remaining bytes per `what`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ProcEventHeader {
    what: u32,
    cpu: u32,
    timestamp_ns: u64,
}

/// `struct fork_proc_event` payload.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ForkPayload {
    parent_pid: i32,
    parent_tgid: i32,
    child_pid: i32,
    child_tgid: i32,
}

/// `struct exec_proc_event` payload.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ExecPayload {
    process_pid: i32,
    process_tgid: i32,
}

/// `struct exit_proc_event` payload.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ExitPayload {
    process_pid: i32,
    process_tgid: i32,
    exit_code: u32,
    exit_signal: u32,
    parent_pid: i32,
    parent_tgid: i32,
}

/// What we surface to the rest of the daemon. PIDs are `u32` here to
/// match `TargetProcess::pid` even though the wire format uses `i32`
/// (kernel pid_t).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcEvent {
    Fork { parent: u32, child: u32 },
    Exec { pid: u32 },
    Exit { pid: u32 },
}

/// Owned socket fd subscribed to the proc connector multicast group.
pub struct ProcConnector {
    fd: OwnedFd,
}

impl ProcConnector {
    /// Open the socket, bind it to the proc connector multicast group,
    /// and send the LISTEN op. Returns Err if any step fails — the
    /// caller is expected to fall back to /proc-walk mode.
    pub fn open() -> Result<Self> {
        // SOCK_DGRAM is what the kernel docs and every working example
        // use. Setting CLOEXEC keeps the fd from leaking across an exec
        // we don't even do, but it's still hygiene.
        let raw_fd = unsafe { socket(AF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC, NETLINK_CONNECTOR) };
        if raw_fd < 0 {
            bail!(
                "socket(AF_NETLINK, NETLINK_CONNECTOR): {}",
                io::Error::last_os_error()
            );
        }
        // SAFETY: socket() returned a valid fd; OwnedFd takes ownership.
        let fd: OwnedFd = unsafe { OwnedFd::from_raw_fd_owned(raw_fd) };

        // Larger SO_RCVBUF reduces the chance of dropped events under
        // an event burst (e.g. cargo test forking lots of processes).
        // 8 MiB is plenty and well below default rmem_max on most distros.
        let bufsize: c_int = 8 * 1024 * 1024;
        let _ = unsafe {
            setsockopt(
                fd.as_raw_fd(),
                SOL_SOCKET,
                SO_RCVBUF,
                &bufsize as *const _ as *const c_void,
                size_of::<c_int>() as u32,
            )
        };

        let mut addr: sockaddr_nl = unsafe { zeroed() };
        addr.nl_family = AF_NETLINK as u16;
        addr.nl_pid = std::process::id();
        addr.nl_groups = CN_IDX_PROC;

        let bind_ret = unsafe {
            bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const _,
                size_of::<sockaddr_nl>() as u32,
            )
        };
        if bind_ret < 0 {
            bail!(
                "bind NETLINK_CONNECTOR group {} (needs CAP_NET_ADMIN on older kernels): {}",
                CN_IDX_PROC,
                io::Error::last_os_error(),
            );
        }

        // Send PROC_CN_MCAST_LISTEN to start the event stream.
        let mut buf = [0u8; size_of::<Nlmsghdr>() + size_of::<CnMsg>() + size_of::<u32>()];
        let total_len = buf.len() as u32;

        // nlmsghdr
        let hdr = Nlmsghdr {
            nlmsg_len: total_len,
            nlmsg_type: 0, // NLMSG_DONE-ish, kernel ignores type for connector
            nlmsg_flags: 0,
            nlmsg_seq: 0,
            nlmsg_pid: std::process::id(),
        };
        // SAFETY: Nlmsghdr is repr(C) and trivially copyable
        unsafe {
            std::ptr::copy_nonoverlapping(
                &hdr as *const _ as *const u8,
                buf.as_mut_ptr(),
                size_of::<Nlmsghdr>(),
            );
        }
        // cn_msg
        let cn = CnMsg {
            id: CbId {
                idx: CN_IDX_PROC,
                val: CN_VAL_PROC,
            },
            seq: 0,
            ack: 0,
            len: size_of::<u32>() as u16,
            flags: 0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &cn as *const _ as *const u8,
                buf.as_mut_ptr().add(size_of::<Nlmsghdr>()),
                size_of::<CnMsg>(),
            );
        }
        // payload: PROC_CN_MCAST_LISTEN as u32
        let op = PROC_CN_MCAST_LISTEN.to_ne_bytes();
        buf[size_of::<Nlmsghdr>() + size_of::<CnMsg>()..].copy_from_slice(&op);

        let sent = unsafe { send(fd.as_raw_fd(), buf.as_ptr() as *const c_void, buf.len(), 0) };
        if sent < 0 {
            bail!("send PROC_CN_MCAST_LISTEN: {}", io::Error::last_os_error());
        }

        Ok(Self { fd })
    }

    /// Exposed for callers that want to plug the socket into their own
    /// poll loop or eventfd group. Currently unused inside the crate.
    #[allow(dead_code)]
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Block on `recv()` until at least one event arrives, then parse
    /// every event packed into the returned datagram. The kernel may
    /// deliver multiple proc_event records in one netlink message under
    /// load, but in practice each datagram carries one event.
    pub fn recv_events(&self, buf: &mut [u8]) -> Result<Vec<ProcEvent>> {
        let n = unsafe {
            recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                0,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            // Treat EINTR as "no events this time" so the polling loop
            // can re-check shutdown flags without surfacing an error.
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            bail!("recv on cn_proc socket: {}", err);
        }
        parse_events(&buf[..n as usize])
    }

    /// Like `recv_events` but never blocks — returns an empty vec when
    /// the socket has no events queued. Useful when we want to drain
    /// before checking a shutdown flag without holding the kernel up.
    #[allow(dead_code)]
    pub fn try_recv_events(&self, buf: &mut [u8]) -> Result<Vec<ProcEvent>> {
        let n = unsafe {
            recv(
                self.fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                MSG_DONTWAIT,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::Interrupted {
                return Ok(Vec::new());
            }
            bail!("recv MSG_DONTWAIT on cn_proc socket: {}", err);
        }
        parse_events(&buf[..n as usize])
    }
}

/// Pure parser: walk the netlink datagram payload and translate each
/// proc_event we recognise into our ProcEvent enum. Pulled out for
/// unit tests that don't need a kernel socket.
pub fn parse_events(packet: &[u8]) -> Result<Vec<ProcEvent>> {
    let mut events = Vec::new();
    let nlh_size = size_of::<Nlmsghdr>();
    let cn_size = size_of::<CnMsg>();
    let evh_size = size_of::<ProcEventHeader>();

    if packet.len() < nlh_size + cn_size + evh_size {
        // Too short to contain even one event — bail silently.
        return Ok(events);
    }

    // The payload starts right after the netlink header. We don't
    // currently need to validate nlmsg_len because the kernel only
    // ever sends well-formed datagrams; if that ever changes we'd
    // walk multiple nlmsg here.
    let payload = &packet[nlh_size..];
    if payload.len() < cn_size + evh_size {
        return Ok(events);
    }
    let event_data = &payload[cn_size..];

    // Read proc_event header
    let mut header = ProcEventHeader {
        what: 0,
        cpu: 0,
        timestamp_ns: 0,
    };
    // SAFETY: event_data has at least evh_size bytes (checked above)
    unsafe {
        std::ptr::copy_nonoverlapping(
            event_data.as_ptr(),
            &mut header as *mut _ as *mut u8,
            evh_size,
        );
    }

    let body = &event_data[evh_size..];
    match header.what {
        PROC_EVENT_FORK if body.len() >= size_of::<ForkPayload>() => {
            let mut p = ForkPayload {
                parent_pid: 0,
                parent_tgid: 0,
                child_pid: 0,
                child_tgid: 0,
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    body.as_ptr(),
                    &mut p as *mut _ as *mut u8,
                    size_of::<ForkPayload>(),
                );
            }
            // Filter to thread-group leaders: per-thread fork events
            // are noise for our purposes (we work at the process level).
            if p.child_pid == p.child_tgid && p.child_pid > 0 {
                events.push(ProcEvent::Fork {
                    parent: p.parent_tgid as u32,
                    child: p.child_tgid as u32,
                });
            }
        }
        PROC_EVENT_EXEC if body.len() >= size_of::<ExecPayload>() => {
            let mut p = ExecPayload {
                process_pid: 0,
                process_tgid: 0,
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    body.as_ptr(),
                    &mut p as *mut _ as *mut u8,
                    size_of::<ExecPayload>(),
                );
            }
            if p.process_tgid > 0 {
                events.push(ProcEvent::Exec {
                    pid: p.process_tgid as u32,
                });
            }
        }
        PROC_EVENT_EXIT if body.len() >= size_of::<ExitPayload>() => {
            let mut p = ExitPayload {
                process_pid: 0,
                process_tgid: 0,
                exit_code: 0,
                exit_signal: 0,
                parent_pid: 0,
                parent_tgid: 0,
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    body.as_ptr(),
                    &mut p as *mut _ as *mut u8,
                    size_of::<ExitPayload>(),
                );
            }
            // Only emit the process-level exit (when the thread-group
            // leader dies); per-thread exits inside a still-living
            // process aren't interesting here.
            if p.process_pid == p.process_tgid && p.process_tgid > 0 {
                events.push(ProcEvent::Exit {
                    pid: p.process_tgid as u32,
                });
            }
        }
        _ => {}
    }

    Ok(events)
}

// std::os::fd::OwnedFd doesn't expose a `from_raw_fd_owned` constructor
// in stable Rust; we wrap the unsafe `from_raw_fd` here so the call
// site reads as the intent ("take ownership of an fd we just created").
trait OwnedFdFromRaw {
    /// SAFETY: `fd` must be a valid open fd that nobody else owns.
    unsafe fn from_raw_fd_owned(fd: RawFd) -> Self;
}

impl OwnedFdFromRaw for OwnedFd {
    unsafe fn from_raw_fd_owned(fd: RawFd) -> Self {
        use std::os::fd::FromRawFd;
        OwnedFd::from_raw_fd(fd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic packet [nlmsghdr][cn_msg][proc_event header][payload]
    /// so we can unit-test `parse_events` without a live kernel socket.
    fn build_packet(what: u32, body: &[u8]) -> Vec<u8> {
        let total =
            size_of::<Nlmsghdr>() + size_of::<CnMsg>() + size_of::<ProcEventHeader>() + body.len();
        let mut buf = vec![0u8; total];
        let mut off = 0;

        let nlh = Nlmsghdr {
            nlmsg_len: total as u32,
            nlmsg_type: 0,
            nlmsg_flags: 0,
            nlmsg_seq: 0,
            nlmsg_pid: 0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &nlh as *const _ as *const u8,
                buf.as_mut_ptr().add(off),
                size_of::<Nlmsghdr>(),
            );
        }
        off += size_of::<Nlmsghdr>();

        let cn = CnMsg {
            id: CbId {
                idx: CN_IDX_PROC,
                val: CN_VAL_PROC,
            },
            seq: 0,
            ack: 0,
            len: (size_of::<ProcEventHeader>() + body.len()) as u16,
            flags: 0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &cn as *const _ as *const u8,
                buf.as_mut_ptr().add(off),
                size_of::<CnMsg>(),
            );
        }
        off += size_of::<CnMsg>();

        let evh = ProcEventHeader {
            what,
            cpu: 0,
            timestamp_ns: 0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                &evh as *const _ as *const u8,
                buf.as_mut_ptr().add(off),
                size_of::<ProcEventHeader>(),
            );
        }
        off += size_of::<ProcEventHeader>();

        buf[off..off + body.len()].copy_from_slice(body);
        buf
    }

    #[test]
    fn parse_fork_event_thread_group_leader() {
        let body: &[i32] = &[100, 100, 200, 200];
        // SAFETY: i32 is repr(C) here too, layout-compatible
        let body_bytes = unsafe {
            std::slice::from_raw_parts(body.as_ptr() as *const u8, std::mem::size_of_val(body))
        };
        let packet = build_packet(PROC_EVENT_FORK, body_bytes);
        let events = parse_events(&packet).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            ProcEvent::Fork {
                parent: 100,
                child: 200
            }
        );
    }

    #[test]
    fn parse_fork_event_skips_per_thread_fork() {
        // child_pid != child_tgid → it's a thread inside a process,
        // not a new process. We do not surface it.
        let body: &[i32] = &[100, 100, 201, 200];
        let body_bytes = unsafe {
            std::slice::from_raw_parts(body.as_ptr() as *const u8, std::mem::size_of_val(body))
        };
        let packet = build_packet(PROC_EVENT_FORK, body_bytes);
        let events = parse_events(&packet).unwrap();
        assert!(events.is_empty(), "per-thread forks must be filtered out");
    }

    #[test]
    fn parse_exec_event() {
        let body: &[i32] = &[300, 300];
        let body_bytes = unsafe {
            std::slice::from_raw_parts(body.as_ptr() as *const u8, std::mem::size_of_val(body))
        };
        let packet = build_packet(PROC_EVENT_EXEC, body_bytes);
        let events = parse_events(&packet).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], ProcEvent::Exec { pid: 300 });
    }

    #[test]
    fn parse_exit_event_thread_group_leader() {
        // [pid, tgid, exit_code, exit_signal, parent_pid, parent_tgid]
        let body: &[i32] = &[400, 400, 0, 0, 100, 100];
        let body_bytes = unsafe {
            std::slice::from_raw_parts(body.as_ptr() as *const u8, std::mem::size_of_val(body))
        };
        let packet = build_packet(PROC_EVENT_EXIT, body_bytes);
        let events = parse_events(&packet).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], ProcEvent::Exit { pid: 400 });
    }

    #[test]
    fn parse_exit_event_skips_per_thread_exit() {
        let body: &[i32] = &[401, 400, 0, 0, 100, 100];
        let body_bytes = unsafe {
            std::slice::from_raw_parts(body.as_ptr() as *const u8, std::mem::size_of_val(body))
        };
        let packet = build_packet(PROC_EVENT_EXIT, body_bytes);
        let events = parse_events(&packet).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_unknown_event_kind_is_ignored() {
        // Random other "what" value (UID change = 0x4)
        let body: &[u8] = &[0u8; 32];
        let packet = build_packet(0x4, body);
        let events = parse_events(&packet).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_short_packet_does_not_panic() {
        let short = [0u8; 8];
        let events = parse_events(&short).unwrap();
        assert!(events.is_empty());
    }
}
