//! Build script: compile the cpu_tracker BPF program and generate a
//! Rust skeleton next to it.
//!
//! Steps:
//!   1. Dump the running kernel's BTF into a fresh vmlinux.h (so the BPF
//!      C source can `#include` the kernel struct definitions it needs).
//!   2. Hand off to libbpf-cargo's SkeletonBuilder, which invokes clang
//!      to compile the BPF object and writes a Rust skeleton inside
//!      `OUT_DIR/cpu_tracker.skel.rs`.
//!
//! Build deps the host needs:
//!   - clang     (compiles BPF target)
//!   - bpftool   (generates vmlinux.h from /sys/kernel/btf/vmlinux)
//!   - libbpf    (linked at runtime by libbpf-rs)

use libbpf_cargo::SkeletonBuilder;
use std::env;
use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "src/bpf/cpu_tracker.bpf.c";

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));

    // 1. Generate vmlinux.h next to the BPF source. Doing it in OUT_DIR
    //    keeps it out of git and makes builds reproducible per host.
    let vmlinux = out_dir.join("vmlinux.h");
    let bpftool_status = Command::new("bpftool")
        .args(["btf", "dump", "file", "/sys/kernel/btf/vmlinux", "format", "c"])
        .stdout(std::fs::File::create(&vmlinux).expect("create vmlinux.h"))
        .status()
        .expect("invoke bpftool — install pacman -S bpf");
    assert!(
        bpftool_status.success(),
        "bpftool btf dump failed (status {bpftool_status})",
    );

    // 2. Compile BPF + generate skeleton. The include path makes the
    //    generated vmlinux.h visible to the BPF source via
    //    `#include "vmlinux.h"`.
    let skel = out_dir.join("cpu_tracker.skel.rs");
    SkeletonBuilder::new()
        .source(SOURCE)
        .clang_args([
            std::ffi::OsString::from("-I"),
            out_dir.as_os_str().to_owned(),
        ])
        .build_and_generate(&skel)
        .expect("build cpu_tracker BPF skeleton");
}
