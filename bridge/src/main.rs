// Stub — full bridge lands in the next commit. The workspace needs this
// file to compile; the daemon UDS transport (previous commit) is
// independently testable without the bridge being functional yet.

fn main() {
    eprintln!("bssl-ram-bridge stub — full native messaging host lands in the next commit");
    std::process::exit(64); // EX_USAGE
}
