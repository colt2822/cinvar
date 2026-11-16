//! Raw-BPF seccomp program that makes the plugin sandbox non-negotiable about Unix sockets.
//!
//! A manifest may declare `network: true`. That buys network egress (DNS, HTTP to a local render
//! box or ComfyUI), so the sandbox keeps the host network namespace. It must not buy the ability to
//! reach host *control* endpoints, and not all of those are files: abstract-namespace Unix sockets
//! live in the network namespace, so `@dbus-*`, `@systemd/*`, docker and gpg-agent endpoints stay
//! reachable by name however thoroughly `/run` is masked (`src/plugins/mod.rs` masks `/run` for
//! their filesystem cousins).
//!
//! bubblewrap installs a filter on the sandboxed process before it execs (`--seccomp`), so we deny
//! the syscall that would create the connection instead of hiding paths: `socket()` for `AF_UNIX`
//! returns EPERM, while `AF_INET`, `AF_INET6` and `AF_NETLINK` (getaddrinfo) keep working. The
//! filter kills on an architecture mismatch, so a 32-bit compat entry point (`int 0x80`) cannot
//! reach the socket syscall through a different number.

use std::path::PathBuf;

/// The file bubblewrap reads its filter program from, inside the CINVAR config directory (per-user,
/// never inside a plugin directory, and masked by a tmpfs inside the sandbox itself).
pub const FILTER_FILE: &str = "sandbox-deny-af-unix.bpf";

const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const AUDIT_ARCH_AARCH64: u32 = 0xC000_00B7;
const SYS_SOCKET_X86_64: u32 = 41;
const SYS_SOCKET_AARCH64: u32 = 198;

const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const RET_EPERM: u32 = 0x0005_0000 | 1; // SECCOMP_RET_ERRNO | EPERM

const AF_UNSPEC: u32 = 0;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const AF_NETLINK: u32 = 16;

const BPF_LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
const BPF_JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
const BPF_RET_K: u16 = 0x06; // BPF_RET | BPF_K

/// One `struct sock_filter`: 2 bytes of opcode, two 1-byte jump offsets, 4 bytes of operand.
#[derive(Clone, Copy)]
struct Insn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

const fn insn(code: u16, jt: u8, jf: u8, k: u32) -> Insn {
    Insn { code, jt, jf, k }
}

/// Offsets into `struct seccomp_data`.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0: u32 = 16;

/// The filter program: deny `socket(AF_UNIX, ...)`, allow the address families a networked plugin
/// legitimately needs. Returned as the exact byte layout bubblewrap reads (`struct sock_filter`
/// records, native endianness), or `None` on an architecture with no known syscall numbers.
pub fn socket_deny_filter() -> Option<Vec<u8>> {
    let (arch, sys_socket) = match std::env::consts::ARCH {
        "x86_64" => (AUDIT_ARCH_X86_64, SYS_SOCKET_X86_64),
        "aarch64" => (AUDIT_ARCH_AARCH64, SYS_SOCKET_AARCH64),
        _ => return None,
    };
    // 0  arch := seccomp_data.arch
    // 1  arch == expected ? continue : kill
    // 2  kill
    // 3  nr := seccomp_data.nr
    // 4  nr == __NR_socket ? continue : allow
    // 5  arg0 := seccomp_data.args[0] (the address family)
    // 6..9  allow the families below, falling into EPERM for everything else
    // 10 EPERM
    // 11 allow
    let program = [
        insn(BPF_LD_W_ABS, 0, 0, OFF_ARCH),
        insn(BPF_JEQ_K, 1, 0, arch),
        insn(BPF_RET_K, 0, 0, RET_KILL_PROCESS),
        insn(BPF_LD_W_ABS, 0, 0, OFF_NR),
        insn(BPF_JEQ_K, 0, 6, sys_socket),
        insn(BPF_LD_W_ABS, 0, 0, OFF_ARG0),
        insn(BPF_JEQ_K, 4, 0, AF_UNSPEC),
        insn(BPF_JEQ_K, 3, 0, AF_INET),
        insn(BPF_JEQ_K, 2, 0, AF_INET6),
        insn(BPF_JEQ_K, 1, 0, AF_NETLINK),
        insn(BPF_RET_K, 0, 0, RET_EPERM),
        insn(BPF_RET_K, 0, 0, RET_ALLOW),
    ];
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for i in program {
        bytes.extend_from_slice(&i.code.to_ne_bytes());
        bytes.push(i.jt);
        bytes.push(i.jf);
        bytes.extend_from_slice(&i.k.to_ne_bytes());
    }
    Some(bytes)
}

/// Materialises the filter program and returns the path bubblewrap's `--seccomp` wrapper opens.
/// `None` means the filter cannot be installed on this machine (architecture without known syscall
/// numbers, or an unwritable config directory); the caller must then fall back to an isolated
/// network namespace rather than keep the host namespace unguarded.
pub fn filter_file() -> Option<PathBuf> {
    let bytes = socket_deny_filter()?;
    let dir = crate::util::paths::config_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(FILTER_FILE);
    if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        std::fs::write(&path, &bytes).ok()?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insn_at(bytes: &[u8], n: usize) -> (u16, u8, u8, u32) {
        let at = n * 8;
        (
            u16::from_ne_bytes([bytes[at], bytes[at + 1]]),
            bytes[at + 2],
            bytes[at + 3],
            u32::from_ne_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]),
        )
    }

    /// Shape only; that the kernel *accepts* this program and enforces it on a sandboxed child is
    /// proven end to end by tests/plugin_sandbox.rs (which fails on unfiltered argv).
    #[test]
    fn the_program_validates_arch_then_denies_af_unix() {
        let Some(bytes) = socket_deny_filter() else {
            return; // unsupported architecture: nothing to assert
        };
        assert_eq!(bytes.len(), 12 * 8, "unexpected instruction count");
        // Architecture is checked first and a mismatch kills the process, so a 32-bit compat
        // entry point cannot reach socket() through a different syscall number.
        assert_eq!(insn_at(&bytes, 0), (BPF_LD_W_ABS, 0, 0, OFF_ARCH));
        assert_eq!(insn_at(&bytes, 1).0, BPF_JEQ_K);
        assert_eq!(insn_at(&bytes, 2).3, RET_KILL_PROCESS);
        // Argument 0 (the address family) is what decides EPERM versus ALLOW.
        assert_eq!(insn_at(&bytes, 4).0, BPF_JEQ_K);
        assert!(
            [SYS_SOCKET_X86_64, SYS_SOCKET_AARCH64].contains(&insn_at(&bytes, 4).3),
            "syscall 4 does not test the architecture's socket number"
        );
        assert_eq!(insn_at(&bytes, 5), (BPF_LD_W_ABS, 0, 0, OFF_ARG0));
        // No jump may leave the program.
        for n in 0..bytes.len() / 8 {
            let (code, jt, jf, _) = insn_at(&bytes, n);
            if code == BPF_JEQ_K {
                for off in [jt, jf] {
                    assert!(
                        (n + 1 + off as usize) < bytes.len() / 8,
                        "jump out of range"
                    );
                }
            }
        }
        assert_eq!(insn_at(&bytes, 10).3, RET_EPERM);
        assert_eq!(insn_at(&bytes, 11).3, RET_ALLOW);
    }
}
