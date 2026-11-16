//! Minimal ustar archive writer/reader.
//!
//! Distributed rendering moves two kinds of payload between the coordinator and an ssh worker: a
//! self-describing work unit (a Python render script plus its manifest) out, and rendered PNG frames
//! back. Both are small and must be byte-exact, so they travel as a tar stream this module builds
//! and parses itself: no shell pipeline, no local tar dependency, no filename quoting, and one
//! round-trip per unit. Reading verifies every header checksum, so a truncated or garbled transfer
//! is a hard error instead of a half-copied frame.

use crate::error::{Error, ErrorCode, Result};

const BLOCK: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarFile {
    pub name: String,
    pub data: Vec<u8>,
}

impl TarFile {
    pub fn new(name: impl Into<String>, data: impl Into<Vec<u8>>) -> TarFile {
        TarFile {
            name: name.into(),
            data: data.into(),
        }
    }
}

fn octal(value: u64, width: usize) -> Vec<u8> {
    // width includes the trailing NUL.
    let digits = format!("{:0width$o}", value, width = width - 1);
    let mut out = digits.into_bytes();
    out.push(0);
    out
}

fn header(name: &str, size: usize, mode: u64) -> Result<[u8; BLOCK]> {
    if name.is_empty() || name.len() > 100 {
        return Err(Error::new(
            ErrorCode::Internal,
            format!("tar member name must be 1..=100 bytes, got {name:?}"),
        ));
    }
    let mut block = [0u8; BLOCK];
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..108].copy_from_slice(&octal(mode, 8));
    block[108..116].copy_from_slice(&octal(0, 8)); // uid
    block[116..124].copy_from_slice(&octal(0, 8)); // gid
    block[124..136].copy_from_slice(&octal(size as u64, 12));
    block[136..148].copy_from_slice(&octal(0, 12)); // mtime
    block[148..156].copy_from_slice(b"        "); // checksum placeholder: spaces
    block[156] = b'0'; // regular file
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");

    let checksum: u32 = block.iter().map(|b| *b as u32).sum();
    let encoded = format!("{:06o}\0 ", checksum);
    block[148..156].copy_from_slice(encoded.as_bytes());
    Ok(block)
}

/// Builds an uncompressed ustar archive containing the given files, in order.
pub fn build(files: &[TarFile]) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    for file in files {
        out.extend_from_slice(&header(&file.name, file.data.len(), 0o644)?);
        out.extend_from_slice(&file.data);
        let pad = (BLOCK - file.data.len() % BLOCK) % BLOCK;
        out.extend(std::iter::repeat(0u8).take(pad));
    }
    out.extend(std::iter::repeat(0u8).take(BLOCK * 2));
    Ok(out)
}

fn parse_octal(raw: &[u8]) -> Option<u64> {
    let text: String = raw
        .iter()
        .take_while(|b| **b != 0 && **b != b' ')
        .map(|b| *b as char)
        .collect();
    u64::from_str_radix(text.trim(), 8).ok()
}

fn check_checksum(block: &[u8]) -> bool {
    let Some(stored) = parse_octal(&block[148..156]) else {
        return false;
    };
    let mut sum: u64 = 0;
    for (index, byte) in block.iter().enumerate() {
        if (148..156).contains(&index) {
            sum += b' ' as u64;
        } else {
            sum += *byte as u64;
        }
    }
    sum == stored
}

/// Parses an archive produced by [build] (or by GNU tar, which uses the same ustar headers).
pub fn read(bytes: &[u8]) -> Result<Vec<TarFile>> {
    let mut files = Vec::new();
    let mut offset = 0usize;
    while offset + BLOCK <= bytes.len() {
        let block = &bytes[offset..offset + BLOCK];
        if block.iter().all(|b| *b == 0) {
            break;
        }
        if !check_checksum(block) {
            return Err(Error::new(
                ErrorCode::Protocol,
                format!("tar header at offset {offset} failed its checksum"),
            ));
        }
        let name_end = block[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&block[..name_end]).trim().to_string();
        let size = parse_octal(&block[124..136]).ok_or_else(|| {
            Error::new(ErrorCode::Protocol, "tar header has an unparseable size")
        })? as usize;
        let typeflag = block[156];
        offset += BLOCK;
        if offset + size > bytes.len() {
            return Err(Error::new(
                ErrorCode::Protocol,
                format!("tar member {name:?} claims {size} bytes but the archive is truncated"),
            ));
        }
        let data = bytes[offset..offset + size].to_vec();
        offset += size + (BLOCK - size % BLOCK) % BLOCK;
        // Directories (and any non-regular member) are skipped: every payload here is a plain file.
        if typeflag == b'0' || typeflag == 0 {
            files.push(TarFile { name, data });
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<TarFile> {
        vec![
            TarFile::new("manifest.json", br#"{"unit":3}"#.to_vec()),
            TarFile::new("empty.bin", Vec::new()),
            TarFile::new("one.bin", vec![0u8]),
            TarFile::new("edge511.bin", vec![0xAB; 511]),
            TarFile::new("edge512.bin", vec![0xCD; 512]),
            TarFile::new("edge513.bin", vec![0xEF; 513]),
            TarFile::new(
                "all_bytes.bin",
                (0..=255u8).cycle().take(4096).collect::<Vec<u8>>(),
            ),
        ]
    }

    #[test]
    fn roundtrip_preserves_every_byte() {
        let files = sample();
        let archive = build(&files).unwrap();
        assert_eq!(archive.len() % BLOCK, 0, "archive is block aligned");
        let read_back = read(&archive).unwrap();
        assert_eq!(read_back, files);
    }

    #[test]
    fn empty_archive_roundtrips() {
        let archive = build(&[]).unwrap();
        assert_eq!(archive.len(), BLOCK * 2);
        assert!(read(&archive).unwrap().is_empty());
    }

    #[test]
    fn corrupted_header_is_rejected() {
        let mut archive = build(&sample()).unwrap();
        archive[0] ^= 0xFF; // damage the name field, invalidating the checksum
        let err = read(&archive).unwrap_err();
        assert_eq!(err.code, ErrorCode::Protocol);
        assert!(err.message.contains("checksum"), "{}", err.message);
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let files = vec![TarFile::new("big.bin", vec![7u8; 4096])];
        let archive = build(&files).unwrap();
        let cut = &archive[..BLOCK + 1000];
        let err = read(cut).unwrap_err();
        assert!(err.message.contains("truncated"), "{}", err.message);
    }

    #[test]
    fn gnu_tar_reads_what_we_write() {
        // Interop guard: the same archive, parsed by the system tar, lists our members.
        let files = sample();
        let archive = build(&files).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.tar");
        std::fs::write(&path, &archive).unwrap();
        let out = crate::util::proc::run(
            std::path::Path::new("tar"),
            &["-tf".to_string(), path.to_string_lossy().to_string()],
            &crate::util::proc::ProcOpts::default().with_timeout(60),
        )
        .unwrap();
        assert!(out.success, "tar failed: {}", out.stderr);
        let listed: Vec<&str> = out.stdout.lines().map(str::trim).collect();
        let expected: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(listed, expected);
    }
}
