use std::hint::black_box;
use std::time::{Duration, Instant};

use iroh_rooms_v2_core::governance::log::Role;
use iroh_rooms_v2_core::ids::{PrincipalId, LEN};
use iroh_rooms_v2_core::member::{
    MemberMapProjection, ProjectedMemberRecord, ProjectedStatus, RoleLabel,
};

const MEMBER_COUNT: u64 = 10_000;
const WARMUPS: usize = 3;
const ITERATIONS: usize = 20;
const REFERENCE_BOUND: Duration = Duration::from_millis(10);
const EXPECTED_ROOT: &str = "51d1ae130a534533d6b808b2f866efb3728f2ac32117f6363c73c7d6025e271a";

fn record(counter: u64) -> ProjectedMemberRecord {
    let mut bytes = [0; LEN];
    bytes[..8].copy_from_slice(&counter.to_be_bytes());
    ProjectedMemberRecord {
        member_id: PrincipalId::from_bytes(bytes),
        status: ProjectedStatus::Active,
        roles: vec![RoleLabel::from(Role::Member)],
        active_devices: Vec::new(),
        grant_seq: counter + 1,
        revoke_seq: None,
        profile: None,
    }
}

fn build(records: &[ProjectedMemberRecord]) -> MemberMapProjection {
    MemberMapProjection::from_records(records.iter().cloned()).expect("valid fixture records")
}

fn percentile(samples: &mut [Duration], numerator: usize, denominator: usize) -> Duration {
    samples.sort_unstable();
    let index = (samples.len() - 1) * numerator / denominator;
    samples[index]
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |version| version.trim().to_owned())
}

fn main() {
    let records: Vec<_> = (0..MEMBER_COUNT).map(record).collect();
    for _ in 0..WARMUPS {
        black_box(build(black_box(&records)));
    }
    let mut samples = Vec::with_capacity(ITERATIONS);
    let mut root = None;
    for _ in 0..ITERATIONS {
        let started = Instant::now();
        let projection = build(black_box(&records));
        samples.push(started.elapsed());
        root = Some(projection.root());
        black_box(projection);
    }
    let median = percentile(&mut samples.clone(), 1, 2);
    let p99 = percentile(&mut samples, 99, 100);
    let root = root.expect("at least one iteration");
    assert_eq!(hex::encode(root.as_bytes()), EXPECTED_ROOT);
    println!(
        "member_merkle_10000 target={} rustc={} warmups={} iterations={} median_us={} p99_us={} reference_bound_us={} root={}",
        std::env::consts::ARCH,
        rustc_version(),
        WARMUPS,
        ITERATIONS,
        median.as_micros(),
        p99.as_micros(),
        REFERENCE_BOUND.as_micros(),
        hex::encode(root.as_bytes())
    );
}
