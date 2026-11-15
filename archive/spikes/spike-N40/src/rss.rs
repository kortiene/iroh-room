//! Process-RSS estimation for the loopback measurement harness (spec §4 D3,
//! §6.6). Every node runs in one process, so Linux cannot report exact RSS
//! per node; instead this module reads total process RSS from
//! `/proc/self/status` (the `VmRSS:` line, reported in kB) and the harness
//! derives `rss_per_node_est = (process_rss - baseline_rss) / N` (spec §4 D3
//! / §6.6).
//!
//! Linux-only by design: `/proc/self/status` exists only on Linux. On any
//! other platform [`process_rss_bytes`] returns a clear error so a non-Linux
//! run fails closed on a missing measurement rather than reporting a
//! fabricated number. The CI host is Linux (spec §10).

use std::fs;

use anyhow::{Context, Result};

/// Read this process's current resident-set size in bytes from
/// `/proc/self/status` (the `VmRSS:` line, reported in kB by the kernel).
///
/// # Errors
/// Returns an error on a non-Linux platform (no `/proc/self/status`), on a
/// read failure, or if the `VmRSS:` line is absent / malformed.
pub fn process_rss_bytes() -> Result<u64> {
    let status = fs::read_to_string("/proc/self/status")
        .context("/proc/self/status is unavailable (this harness is Linux-only)")?;
    let kb = parse_vm_rss_kib(&status).context("parsing /proc/self/status VmRSS")?;
    Ok(kb.saturating_mul(1024))
}

/// Parse the `VmRSS:` value (in KiB) out of a `/proc/self/status` body.
/// Exposed so the unit test can feed representative lines without depending on
/// the live kernel file.
///
/// # Errors
/// Returns an error if no `VmRSS:` line exists or the value is not a valid
/// base-10 integer.
fn parse_vm_rss_kib(status: &str) -> Result<u64> {
    for line in status.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("VmRSS:") {
            let value = rest
                .split_whitespace()
                .next()
                .context("VmRSS line has no value token")?;
            return value
                .parse::<u64>()
                .with_context(|| format!("VmRSS value is not a u64: {value:?}"));
        }
    }
    Err(anyhow::anyhow!("no VmRSS: line in /proc/self/status"))
}

#[cfg(test)]
mod tests {
    use super::parse_vm_rss_kib;

    #[test]
    fn parses_a_typical_vmrss_line() {
        // Representative excerpt of /proc/self/status. The unit suffix `kB`
        // follows the value; only the integer token is parsed.
        let status = "\
Name:   n40-probe
Umask:  0022
State:  R (running)
VmPeak:   123456 kB
VmSize:   110000 kB
VmLck:         0 kB
VmPin:         0 kB
VmHWM:     23456 kB
VmRSS:     21000 kB
";
        assert_eq!(parse_vm_rss_kib(status).unwrap(), 21_000);
    }

    #[test]
    fn parses_first_match_when_two_vmrss_lines_appear() {
        let status = "VmRSS: 100 kB\njunk: VmRSS: 200 kB\n";
        assert_eq!(parse_vm_rss_kib(status).unwrap(), 100);
    }

    #[test]
    fn errors_when_no_vmrss_line_present() {
        let status = "Name: n40-probe\nState: R\n";
        assert!(parse_vm_rss_kib(status).is_err());
    }

    #[test]
    fn errors_when_vmrss_value_is_not_an_integer() {
        let status = "VmRSS: resident kB\n";
        assert!(parse_vm_rss_kib(status).is_err());
    }

    #[test]
    fn errors_on_empty_input() {
        assert!(parse_vm_rss_kib("").is_err());
    }

    // Integration: this host is Linux (the CI host is Linux per spec §10), so
    // the live read must succeed and return a sane positive byte count. Not
    // marked `#[ignore]`: the entire harness depends on this read working on
    // Linux, so a regression must fail CI loudly. On non-Linux hosts the read
    // is allowed to fail (the harness is Linux-only by design).
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn process_rss_bytes_succeeds_on_linux_ci_host() {
        match super::process_rss_bytes() {
            Ok(bytes) => {
                assert!(
                    bytes >= 1_000_000,
                    "process RSS implausibly small: {bytes} bytes"
                );
            }
            Err(e) => {
                assert!(
                    !cfg!(target_os = "linux"),
                    "process_rss_bytes failed on a Linux host: {e:#}"
                );
            }
        }
    }
}
