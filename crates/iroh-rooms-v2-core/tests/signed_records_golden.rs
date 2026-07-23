//! Frozen golden-vector suite for every v2 signed record and domain-separated
//! hash boundary (issue #153, spec `v2-signed-record-golden-vectors.md`).
//!
//! These tests pin **byte-exact** deterministic-CBOR encodings (CSBs),
//! BLAKE3 domain-separated identifiers, Ed25519 signatures, Merkle boundaries,
//! round-trip equality, the domain-string / canonical-CBOR fence, and one typed
//! negative vector per `Reject` code. The frozen values live as compile-time
//! constants below and are mirrored in [`golden/v2-signed-records.json`] (loaded
//! via `include_str!` so a missing fixture fails the build).
//!
//! Changing any frozen value requires an explicit schema-version bump (see
//! [`golden/README.md`]). This is the acceptance fence for v2 interop: if a
//! domain string, canonical-CBOR rule, id derivation, or signature drifts, at
//! least one test here fails.
//!
//! All signing keys are deterministic public test seeds (non-secret); no
//! entropy, network, store, or real user data is involved.

#![allow(clippy::unwrap_used)]

use iroh_rooms_v2_core::cbor::{self, CborValue};
use iroh_rooms_v2_core::content::body::decode_verified as decode_content;
use iroh_rooms_v2_core::content::{ContentEventBody, ContentKind};
use iroh_rooms_v2_core::domain;
use iroh_rooms_v2_core::governance::approval::decode_verified as decode_approval;
use iroh_rooms_v2_core::governance::model::decode_verified as decode_entry;
use iroh_rooms_v2_core::governance::{
    approval_id, authorize_governance_entry, entry_id, snapshot_hash, validate_against_state,
    ApprovalBody, CheckpointBody, ForkResolutionBody, ForkResolveAction, GovernanceAction,
    GovernanceEntryBody, GovernanceFold, Role,
};
use iroh_rooms_v2_core::governance::{decode_checkpoint, decode_fork_resolution};
use iroh_rooms_v2_core::ids::{
    ApprovalId, ContentEventId, DeviceId, GovernanceEntryId, MemberId, RoomId, SnapshotHash, LEN,
};
use iroh_rooms_v2_core::keys::Signature;
use iroh_rooms_v2_core::keys::SigningKey;
use iroh_rooms_v2_core::member::merkle::{leaf_hash, map_key, value_hash};
use iroh_rooms_v2_core::member::project;
use iroh_rooms_v2_core::signed::{self, Envelope, SignedBody};
use iroh_rooms_v2_core::Reject;

// Pull in the frozen fixtures so a missing/malformed file fails at compile time.
const GOLDEN_JSON: &str = include_str!("golden/v2-signed-records.json");
const GOLDEN_README: &str = include_str!("golden/README.md");

const SCHEMA: u64 = 2;

// ============================================================================
// Frozen values — mirror of `golden/v2-signed-records.json`. ANY change here
// requires a schema-version bump (see `golden/README.md`).
// ============================================================================

// Principals (public keys of the deterministic seed-derived signing keys).
const ADMIN_ID: &str = "b533d8ad9fcfbdde0b481c1b334ddc3c53412fd614564e7e5afd020368d382c3";
const APPROVER_ID: &str = "5b9437adeaffbe8f41b13d96ed49d2f51cd6c266cd8ecc284b0552ec4912b8dd";
const AUTHOR_ID: &str = "e72c28fe718e3a30afc47438da779d508d2dad5a265fafeb4f377e1d57fb098c";
const RESOLVER_ID: &str = "9ff6204d61b59a9e61afdd64fdf294bfe8a16687ba0538823ba59db6cb7b21ff";

// Community/room id derivation.
const COMMUNITY_PAYLOAD_HEX: &str =
    "7070707070707070707070707070707070707070707070707070707070707070";
const COMMUNITY_DERIVED_HASH_HEX: &str =
    "059f8824f6a2f7181f99feeba2355a8e49bd71594fcd29f8fdfce846ebba0297";

// Governance entry (InitRoom).
const ENTRY_CSB: &str = "a76373657101646b696e6469696e69745f726f6f6d6565706f63681903e866616374696f6ea36561646d696e5820b533d8ad9fcfbdde0b481c1b334ddc3c53412fd614564e7e5afd020368d382c369726f6f6d5f6e616d656b676f6c64656e2d726f6f6d6c61646d696e5f6465766963655820b533d8ad9fcfbdde0b481c1b334ddc3c53412fd614564e7e5afd020368d382c366617574686f725820b533d8ad9fcfbdde0b481c1b334ddc3c53412fd614564e7e5afd020368d382c367726f6f6d5f6964582070707070707070707070707070707070707070707070707070707070707070706e736368656d615f76657273696f6e02";
const ENTRY_ID: &str = "blake3:8d8b827177834cd49d6c7e0543c94bef4a245ea3747b288ebf9e47ee20021019";
const ENTRY_SIG: &str = "60435807094bb6b4b0d8c58d710926a58708829d1f464b4ae83fa304080ab92adef037a2d72c71c5954e0333ad63b92122efd4410a0e1732cb7b6487eef6bf08";

// AddMember entry id (referenced by approval + checkpoint).
const ADD_ENTRY_ID: &str =
    "blake3:0b75b61c5c53a7afb9dfca9a894ae387e5f10a966061201d3a3726c1edd0f690";

// Governance approval.
const APPROVAL_CSB: &str = "a66565706f63681903ea67726f6f6d5f69645820707070707070707070707070707070707070707070707070707070707070707068617070726f76657258205b9437adeaffbe8f41b13d96ed49d2f51cd6c266cd8ecc284b0552ec4912b8dd68656e7472795f696458200b75b61c5c53a7afb9dfca9a894ae387e5f10a966061201d3a3726c1edd0f6906e736368656d615f76657273696f6e027370726f706f7365645f73746174655f726f6f74582066ff86bbf84c9accba8250719c797a070a888c15e9d60f03a31f3e58a86ffd01";
const APPROVAL_ID_STR: &str =
    "blake3:d0ef79f3a87f2a5536e2b70d0acd522a070e033026702c7e24cd56928b20c784";
const APPROVAL_SIG: &str = "b7eaee69879b0a90027cd3865d020e7b7e00f9d6bb846a1bf649f0a6825c26aa4f502bd654da2ebfd99d3ff1d85f69dd0f3d3dab78d474271d1ec08223318102";
const PROPOSED_STATE_ROOT: &str =
    "blake3:66ff86bbf84c9accba8250719c797a070a888c15e9d60f03a31f3e58a86ffd01";

// Governance checkpoint.
const CHECKPOINT_CSB: &str = "a763736571016565706f63681903eb67726f6f6d5f6964582070707070707070707070707070707070707070707070707070707070707070706a73746174655f726f6f74582066ff86bbf84c9accba8250719c797a070a888c15e9d60f03a31f3e58a86ffd016b6d656d6265725f726f6f7458200cf0f8e94b408f8fb352fba8ab3de37e0fea74a04250366e45bfc361e892dcad6e676f7665726e616e63655f74697058200b75b61c5c53a7afb9dfca9a894ae387e5f10a966061201d3a3726c1edd0f6906e736368656d615f76657273696f6e02";
const CHECKPOINT_ID: &str =
    "blake3:033bdad5db6ac98d308277c101195b16ef0139062fb77d66487840494366e748";
const CHECKPOINT_SIG: &str = "12d8ab5e1c459578a0b8a1928e01edacfb5afb6f478ea85c0c98849b5fad376ccfef5d0bacc02228e9cbb441996d8fde7fc2d28bb669dd1c8bc2d05242eba208";
const MEMBER_ROOT: &str = "blake3:0cf0f8e94b408f8fb352fba8ab3de37e0fea74a04250366e45bfc361e892dcad";

// Member leaf projection (admin).
const LEAF_CSB: &str = "a564726f6c656561646d696e6673746174757366616374697665696d656d6265725f69645820b533d8ad9fcfbdde0b481c1b334ddc3c53412fd614564e7e5afd020368d382c36b6465766963655f6b657973815820b533d8ad9fcfbdde0b481c1b334ddc3c53412fd614564e7e5afd020368d382c371676f7665726e616e63655f637572736f7258208d8b827177834cd49d6c7e0543c94bef4a245ea3747b288ebf9e47ee20021019";
const LEAF_MERKLE_KEY: &str = "151402008fcf1fc9c75c67b71495db4b4d9a20eb8e11736b9f0a0b193ce8bcdc";
const LEAF_VALUE_HASH: &str = "2b2d345d2bdf96a05ef89adb7b7885fcccb47fd53b61a5947dd9d4ba926b7c0b";
const LEAF_HASH: &str = "b90ccd49a192a83f525d7b64b5f1e54fff549d24a7de0d1fbafec8585adeb9f1";

// Content event (message.text).
const CONTENT_CSB: &str = "a664626f6479a164626f64796f68656c6c6f20676f6c64656e207632646b696e646c6d6573736167652e7465787466617574686f725820e72c28fe718e3a30afc47438da779d508d2dad5a265fafeb4f377e1d57fb098c67726f6f6d5f6964582070707070707070707070707070707070707070707070707070707070707070706776657273696f6e016e736368656d615f76657273696f6e02";
const CONTENT_ID: &str = "blake3:971c4a91a3378325172401fd6ed2b3918512e0d088650406f7bfb11bcbff6365";
const CONTENT_SIG: &str = "27d132739107b0a32991182004bddef45e187aa2f084f63415dd493fb14b7751f37302f7a1e7444b4c5da4f8579799d2f294511fa328735eba9a735f9219fb06";

// Fork resolution (Accept).
const FORK_CSB: &str = "a66565706f63681907d266616374696f6ea26474797065666163636570746677696e6e6572582013f6b252aeff477ee7f76251297106e53fc4cf1aac729a4174652952b140a947667369676e657258209ff6204d61b59a9e61afdd64fdf294bfe8a16687ba0538823ba59db6cb7b21ff67726f6f6d5f6964582070707070707070707070707070707070707070707070707070707070707070706865766964656e636582582013f6b252aeff477ee7f76251297106e53fc4cf1aac729a4174652952b140a947582061b78faf39c5162f22a6f47f1c32d58579c043aa3460afef5aefc2353ca9a56e6e736368656d615f76657273696f6e02";
const FORK_ID: &str = "blake3:3fea904abb212ce328f59487d9e18738ba7f51ed55234fa79dda492e89219377";
const FORK_SIG: &str = "f861c0ed7d3d93762832759aab92b9a8d7b2e579a70c22526b3b13e478056598ca9dddb1fcf2c19fb3f0a5ae39a70b22a66f28c70850464770f4c55fef8bb909";
const FORK_EVIDENCE_0: &str =
    "blake3:13f6b252aeff477ee7f76251297106e53fc4cf1aac729a4174652952b140a947";
const FORK_EVIDENCE_1: &str =
    "blake3:61b78faf39c5162f22a6f47f1c32d58579c043aa3460afef5aefc2353ca9a56e";

// The set of record-type tags every positive suite must cover (spec §5 Step 7).
const POSITIVE_RECORD_TYPES: &[&str] = &[
    "community_id",
    "governance_entry",
    "governance_approval",
    "governance_checkpoint",
    "member_record",
    "content_event",
    "fork_resolution",
];

// Codes currently declared in `Reject` but emitted by NO public path. Per spec
// §5 Step 6 we do not fabricate vectors for them; the completeness test below
// pins this list so a real vector must be added the moment a path appears.
const BLOCKED_CODES: &[&str] = &["wrong_domain", "invalid_approval"];

// ============================================================================
// Helpers
// ============================================================================

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("frozen fixture hex must be valid lowercase")
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; LEN])
}

fn room() -> RoomId {
    RoomId::from_bytes([0x70; LEN])
}

/// Decode a frozen CSB and assert re-encoding is byte-identical (round-trip).
fn assert_csb_round_trip(csb_hex: &str) {
    let bytes = hx(csb_hex);
    let value = cbor::decode_canonical(&bytes).expect("frozen CSB must decode canonically");
    let reencoded = cbor::encode(&value);
    assert_eq!(
        reencoded, bytes,
        "round-trip failed: encode(decode(csb)) != csb for {csb_hex}"
    );
}

/// Assert a fixture's domain string equals the constant in `domain.rs`.
fn assert_domain(name: &str, fixture: &str, constant: &[u8]) {
    let const_str = std::str::from_utf8(constant).expect("domain constants are ASCII");
    assert_eq!(
        fixture, const_str,
        "domain fence: fixture domain for {name} drifted from domain.rs constant"
    );
}

// ============================================================================
// Shared record builders (deterministic; produce the frozen bytes above)
// ============================================================================

fn admin_key() -> SigningKey {
    key(0xa0)
}

fn genesis_entry() -> GovernanceEntryBody {
    let admin = admin_key();
    GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 1,
        parent: None,
        epoch: 1_000,
        action: GovernanceAction::InitRoom {
            admin: admin.member_id(),
            admin_device: admin.device_id(),
            room_name: "golden-room".to_owned(),
        },
    }
}

fn add_member_entry() -> GovernanceEntryBody {
    let admin = admin_key();
    let member = key(0xb0);
    GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(ENTRY_ID.parse().unwrap()),
        epoch: 1_001,
        action: GovernanceAction::AddMember {
            member: member.member_id(),
            device: member.device_id(),
            role: Role::Member,
        },
    }
}

/// Fold genesis + add-member; the outcome whose roots the approval/checkpoint
/// vectors commit to.
fn folded_outcome() -> iroh_rooms_v2_core::governance::FoldOutcome {
    GovernanceFold::new()
        .entry(genesis_entry())
        .entry(add_member_entry())
        .finish()
        .unwrap()
}

// ===========================================================================
// §1 Positive vectors — community / room id derivation
// ===========================================================================

#[test]
fn positive_community_id_room_id_derivation() {
    // Pin the ROOM_ID domain string (D4 fence).
    assert_domain("ROOM_ID", "iroh-rooms:v2:room-id:v1", domain::ROOM_ID);

    let payload = room();
    let derived = domain::blake3_domain(domain::ROOM_ID, payload.as_bytes());
    assert_eq!(
        hex::encode(derived),
        COMMUNITY_DERIVED_HASH_HEX,
        "community/room id derivation drifted"
    );
    // The payload is the room-id bytes themselves (D5: CommunityId == RoomId today).
    assert_eq!(hex::encode(payload.as_bytes()), COMMUNITY_PAYLOAD_HEX);
}

// ===========================================================================
// §2 Positive vector — GovernanceEntry (InitRoom)
// ===========================================================================

#[test]
fn positive_governance_entry_init_room() {
    let admin = admin_key();
    let body = genesis_entry();

    // (1) CSB equality (D3: CSB is the primary frozen artifact).
    let csb = signed::to_csb(&body);
    assert_eq!(hex::encode(&csb), ENTRY_CSB, "governance entry CSB drifted");

    // (2) Strict decode + (3) round-trip byte identity.
    assert_csb_round_trip(ENTRY_CSB);

    // (4) Domain-separated id.
    assert_eq!(entry_id(&body).to_string(), ENTRY_ID);

    // (5) Signing message is SIGN_CONTEXT || CSB.
    let msg = domain::signing_message(domain::GOVERNANCE_ENTRY_SIGN, &csb);
    let mut expect = domain::GOVERNANCE_ENTRY_SIGN.to_vec();
    expect.extend_from_slice(&csb);
    assert_eq!(msg, expect);

    // (6) Ed25519 signature.
    let sig = signed::sign(&body, &admin);
    assert_eq!(sig.to_string(), ENTRY_SIG);

    // (7) Full decode_verified succeeds and returns the expected body.
    let env = signed::seal(&body, &admin);
    assert_eq!(env.id.to_string(), ENTRY_ID);
    assert_eq!(env.signer.to_string(), ADMIN_ID);
    let decoded = decode_entry(&env).expect("entry verifies");
    assert_eq!(decoded, body);
}

// ===========================================================================
// §3 Positive vector — GovernanceApproval
// ===========================================================================

#[test]
fn positive_governance_approval_add_member() {
    let approver = key(0xc0);
    let outcome = folded_outcome();
    let body = ApprovalBody {
        schema_version: SCHEMA,
        room_id: room(),
        entry_id: ADD_ENTRY_ID.parse().unwrap(),
        approver: approver.member_id(),
        proposed_state_root: Some(outcome.state_root),
        epoch: 1_002,
    };

    let csb = signed::to_csb(&body);
    assert_eq!(hex::encode(&csb), APPROVAL_CSB, "approval CSB drifted");
    assert_csb_round_trip(APPROVAL_CSB);
    assert_eq!(approval_id(&body).to_string(), APPROVAL_ID_STR);
    assert_eq!(
        outcome.state_root.to_string(),
        PROPOSED_STATE_ROOT,
        "proposed state root drifted"
    );

    let sig = signed::sign(&body, &approver);
    assert_eq!(sig.to_string(), APPROVAL_SIG);

    let env = signed::seal(&body, &approver);
    assert_eq!(env.signer.to_string(), APPROVER_ID);
    let decoded = decode_approval(&env).expect("approval verifies");
    assert_eq!(decoded, body);
}

// ===========================================================================
// §4 Positive vector — GovernanceCheckpoint
// ===========================================================================

#[test]
fn positive_governance_checkpoint_clean_state() {
    let admin = admin_key();
    let outcome = folded_outcome();
    let body = CheckpointBody {
        schema_version: SCHEMA,
        room_id: outcome.room_id,
        state_root: outcome.state_root,
        member_root: outcome.member_root,
        governance_tip: Some(ADD_ENTRY_ID.parse().unwrap()),
        unresolved_forks: Vec::new(),
        epoch: 1_003,
        seq: 1,
    };

    let csb = signed::to_csb(&body);
    assert_eq!(hex::encode(&csb), CHECKPOINT_CSB, "checkpoint CSB drifted");
    assert_csb_round_trip(CHECKPOINT_CSB);
    assert_eq!(snapshot_hash(&body).to_string(), CHECKPOINT_ID);
    assert_eq!(outcome.state_root.to_string(), PROPOSED_STATE_ROOT);
    assert_eq!(outcome.member_root.to_string(), MEMBER_ROOT);

    let env = signed::seal(&body, &admin);
    assert_eq!(env.id.to_string(), CHECKPOINT_ID);
    let sig = signed::sign(&body, &admin);
    assert_eq!(sig.to_string(), CHECKPOINT_SIG);

    // decode + validate against the folded state both succeed.
    let decoded = decode_checkpoint(&env).expect("checkpoint decodes");
    assert_eq!(decoded, body);
    validate_against_state(&env, &outcome.state).expect("checkpoint validates against state");
}

// ===========================================================================
// §5 Positive vector — MemberRecord / MemberLeaf projection (Merkle boundary)
// ===========================================================================

#[test]
fn positive_member_record_active_member_leaf() {
    let admin = admin_key();
    let outcome = folded_outcome();
    let (mroot, proj) = project(&outcome.state);

    assert_eq!(mroot.to_string(), MEMBER_ROOT, "member root drifted");

    let leaf = proj
        .members
        .iter()
        .find(|m| m.member_id == admin.member_id())
        .expect("admin is projected");

    // Leaf canonical CBOR (D6: projection is the pinned Merkle boundary).
    let leaf_cbor = cbor::encode(&leaf.to_cbor());
    assert_eq!(hex::encode(&leaf_cbor), LEAF_CSB, "member leaf CSB drifted");
    assert_csb_round_trip(LEAF_CSB);

    // Merkle key / value hash / leaf hash.
    let mk = map_key(admin.member_id().as_bytes());
    assert_eq!(hex::encode(mk), LEAF_MERKLE_KEY);
    let vh = value_hash(&leaf.to_cbor());
    assert_eq!(hex::encode(vh), LEAF_VALUE_HASH);
    let lh = leaf_hash(&mk, &vh);
    assert_eq!(hex::encode(lh), LEAF_HASH);

    // Inclusion proof verifies against the root (round-trip proof).
    let proof = proj
        .map
        .prove_inclusion(admin.member_id().as_bytes())
        .expect("admin leaf is set");
    proof
        .verify(&proj.root, true)
        .expect("inclusion proof verifies against frozen root");
}

// ===========================================================================
// §6 Positive vector — ContentEvent (message.text)
// ===========================================================================

#[test]
fn positive_content_event_message_text() {
    let author = key(0xd0);
    let body = ContentEventBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: author.member_id(),
        kind: ContentKind::MessageText,
        version: 1,
        stream_id: None,
        body: CborValue::Map(vec![(
            "body".to_owned(),
            CborValue::Text("hello golden v2".to_owned()),
        )]),
    };

    let csb = signed::to_csb(&body);
    assert_eq!(hex::encode(&csb), CONTENT_CSB, "content event CSB drifted");
    assert_csb_round_trip(CONTENT_CSB);
    assert_eq!(signed::id_of(&body).to_string(), CONTENT_ID);

    let sig = signed::sign(&body, &author);
    assert_eq!(sig.to_string(), CONTENT_SIG);

    let env = signed::seal(&body, &author);
    assert_eq!(env.signer.to_string(), AUTHOR_ID);
    let decoded = decode_content(&env).expect("content event verifies");
    assert_eq!(decoded, body);
}

// ===========================================================================
// §7 Positive vector — SignedForkResolution (Accept)
// ===========================================================================

#[test]
fn positive_fork_resolution_accept_winner() {
    let resolver = key(0xe0);
    let g = genesis_entry();
    let gid = entry_id(&g);

    // Two conflicting admin entries at seq 2 / same parent (fork evidence).
    let branch_a = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin_key().member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 2_001,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x01; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let branch_b = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin_key().member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 2_001,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x02; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let mut evidence = [entry_id(&branch_a), entry_id(&branch_b)];
    evidence.sort();
    let winner = evidence[0];
    assert_eq!(evidence[0].to_string(), FORK_EVIDENCE_0);
    assert_eq!(evidence[1].to_string(), FORK_EVIDENCE_1);

    let body = ForkResolutionBody {
        schema_version: SCHEMA,
        room_id: room(),
        signer: resolver.member_id(),
        evidence,
        action: ForkResolveAction::Accept { winner },
        epoch: 2_002,
    };

    let csb = signed::to_csb(&body);
    assert_eq!(hex::encode(&csb), FORK_CSB, "fork resolution CSB drifted");
    assert_csb_round_trip(FORK_CSB);
    assert_eq!(signed::id_of(&body).to_string(), FORK_ID);

    let sig = signed::sign(&body, &resolver);
    assert_eq!(sig.to_string(), FORK_SIG);

    let env = signed::seal(&body, &resolver);
    assert_eq!(env.signer.to_string(), RESOLVER_ID);
    let decoded = decode_fork_resolution(&env).expect("fork resolution verifies");
    assert_eq!(decoded, body);
}

// ===========================================================================
// §8 Round-trip equality for every standalone canonical object (spec §5 Step 4)
// ===========================================================================

#[test]
fn round_trip_all_frozen_csbs() {
    for (name, csb_hex) in [
        ("entry", ENTRY_CSB),
        ("approval", APPROVAL_CSB),
        ("checkpoint", CHECKPOINT_CSB),
        ("member_leaf", LEAF_CSB),
        ("content", CONTENT_CSB),
        ("fork_resolution", FORK_CSB),
    ] {
        let bytes = hx(csb_hex);
        let value = cbor::decode_canonical(&bytes)
            .unwrap_or_else(|e| panic!("round-trip {name}: canonical decode failed: {e}"));
        assert_eq!(
            cbor::encode(&value),
            bytes,
            "round-trip {name}: re-encode != original"
        );
    }
}

// ===========================================================================
// §9 Domain-string fence (D4): every fixture domain must equal the constant.
// ===========================================================================

#[test]
fn all_domain_constants_match_golden_vectors() {
    assert_domain(
        "GOVERNANCE_ENTRY_SIGN",
        "iroh-rooms:v2:governance-entry:sign:v1",
        domain::GOVERNANCE_ENTRY_SIGN,
    );
    assert_domain(
        "GOVERNANCE_ENTRY_ID",
        "iroh-rooms:v2:governance-entry:id:v1",
        domain::GOVERNANCE_ENTRY_ID,
    );
    assert_domain(
        "GOVERNANCE_APPROVAL_SIGN",
        "iroh-rooms:v2:governance-approval:sign:v1",
        domain::GOVERNANCE_APPROVAL_SIGN,
    );
    assert_domain(
        "GOVERNANCE_APPROVAL_ID",
        "iroh-rooms:v2:governance-approval:id:v1",
        domain::GOVERNANCE_APPROVAL_ID,
    );
    assert_domain(
        "CONTENT_EVENT_SIGN",
        "iroh-rooms:v2:content-event:sign:v1",
        domain::CONTENT_EVENT_SIGN,
    );
    assert_domain(
        "CONTENT_EVENT_ID",
        "iroh-rooms:v2:content-event:id:v1",
        domain::CONTENT_EVENT_ID,
    );
    assert_domain("ROOM_ID", "iroh-rooms:v2:room-id:v1", domain::ROOM_ID);
    assert_domain(
        "GOVERNANCE_STATE_ROOT",
        "iroh-rooms:v2:governance-state-root:v1",
        domain::GOVERNANCE_STATE_ROOT,
    );
    assert_domain(
        "CHECKPOINT_SIGN",
        "iroh-rooms:v2:checkpoint:sign:v1",
        domain::CHECKPOINT_SIGN,
    );
    assert_domain(
        "SNAPSHOT_HASH",
        "iroh-rooms:v2:snapshot-hash:v1",
        domain::SNAPSHOT_HASH,
    );
    assert_domain(
        "MERKLE_EMPTY",
        "iroh-rooms:v2:merkle:empty:v1",
        domain::MERKLE_EMPTY,
    );
    // The two frozen `#134 §6.2` domains the Merkle impl hashes with (PR #176
    // migrated leaf_hash/node_hash onto these; the pinned LEAF_HASH / MEMBER_ROOT
    // vectors above are computed under them).
    assert_domain(
        "MEMBER_LEAF",
        "iroh-room-v2/member-leaf",
        domain::MEMBER_LEAF,
    );
    assert_domain(
        "MERKLE_NODE",
        "iroh-room-v2/merkle-node",
        domain::MERKLE_NODE,
    );
    assert_domain(
        "MERKLE_LEAF",
        "iroh-rooms:v2:merkle:leaf:v1",
        domain::MERKLE_LEAF,
    );
    assert_domain(
        "LEGACY_MERKLE_NODE",
        "iroh-rooms:v2:merkle:node:v1",
        domain::LEGACY_MERKLE_NODE,
    );
    assert_domain(
        "MERKLE_KEY",
        "iroh-rooms:v2:merkle:key:v1",
        domain::MERKLE_KEY,
    );
    assert_domain(
        "FORK_RESOLVE_SIGN",
        "iroh-rooms:v2:fork-resolve:sign:v1",
        domain::FORK_RESOLVE_SIGN,
    );
    assert_domain(
        "FORK_RESOLVE_ID",
        "iroh-rooms:v2:fork-resolve:id:v1",
        domain::FORK_RESOLVE_ID,
    );
}

// ===========================================================================
// §10 Canonical-CBOR fence (Step 5): unsorted keys + non-shortest int reject.
// ===========================================================================

#[test]
fn canonical_cbor_rules_are_fenced_by_vectors() {
    // Unsorted map keys: `bb` before `aa` (bytewise descending) → reject.
    let unsorted = hx("a26162620161 6161 01".replace(' ', "").as_str());
    assert!(cbor::decode_canonical(&unsorted).is_err());

    // Non-shortest integer: 0x1817 encodes 23 in two bytes (23 fits in one).
    let non_shortest = hx("1817");
    assert_eq!(
        cbor::decode_canonical(&non_shortest),
        Err(cbor::CborError::NonShortestInt)
    );

    // The positive entry CSB starts with an `a7` map head (7 entries) — pin the
    // head so a map-width change is caught independently of field contents.
    let entry_bytes = hx(ENTRY_CSB);
    assert_eq!(
        entry_bytes[0], 0xa7,
        "governance entry CSB must open with a 7-entry map head"
    );
}

// ===========================================================================
// §11 Negative vectors — one per Reject::code() (spec §5 Step 6 / D7)
// ===========================================================================

#[test]
fn negative_non_canonical_encoding() {
    // Append a trailing byte to a canonical CSB → strict decode rejects before
    // any body check.
    let mut bad = hx(ENTRY_CSB);
    bad.push(0x00);
    let value_err = cbor::decode_canonical(&bad).unwrap_err();
    assert_eq!(value_err, cbor::CborError::TrailingData);

    // The envelope path surfaces this as NonCanonicalEncoding.
    let admin = admin_key();
    let env = signed::seal(&genesis_entry(), &admin);
    let mut tampered = env.clone();
    tampered.signed.push(0x00);
    assert_eq!(
        decode_entry(&tampered).err(),
        Some(Reject::NonCanonicalEncoding)
    );
}

#[test]
fn negative_unknown_version() {
    let admin = admin_key();
    let mut body = genesis_entry();
    body.schema_version = 99; // signature/id still match this CSB
    let env = signed::seal(&body, &admin);
    assert_eq!(decode_entry(&env).err(), Some(Reject::UnknownVersion));
}

#[test]
fn negative_unknown_record_kind() {
    let admin = admin_key();
    // Hand-build an entry CSB with an unknown `kind`, otherwise valid. The id
    // and signature are computed over this exact CSB so verify reaches the body
    // check, where action_from_cbor rejects the unknown discriminant.
    let raw = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(SCHEMA)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(room().as_bytes().to_vec()),
        ),
        (
            "author".to_owned(),
            CborValue::Bytes(admin.member_id().as_bytes().to_vec()),
        ),
        ("seq".to_owned(), CborValue::Uint(1)),
        ("epoch".to_owned(), CborValue::Uint(1_000)),
        ("kind".to_owned(), CborValue::Text("frobnicate".to_owned())),
        ("action".to_owned(), CborValue::Map(vec![])),
    ]);
    let csb = cbor::encode(&raw);
    let id =
        GovernanceEntryId::from_bytes(domain::blake3_domain(domain::GOVERNANCE_ENTRY_ID, &csb));
    let sig = admin.sign(&domain::signing_message(
        domain::GOVERNANCE_ENTRY_SIGN,
        &csb,
    ));
    let env = Envelope {
        id,
        signed: csb,
        sig,
        signer: admin.member_id(),
    };
    assert_eq!(decode_entry(&env).err(), Some(Reject::UnknownRecordKind));
}

#[test]
fn negative_unknown_content_kind() {
    let author = key(0xd0);
    let raw = CborValue::Map(vec![
        ("schema_version".to_owned(), CborValue::Uint(SCHEMA)),
        (
            "room_id".to_owned(),
            CborValue::Bytes(room().as_bytes().to_vec()),
        ),
        (
            "author".to_owned(),
            CborValue::Bytes(author.member_id().as_bytes().to_vec()),
        ),
        (
            "kind".to_owned(),
            CborValue::Text("message.unknown".to_owned()),
        ),
        ("version".to_owned(), CborValue::Uint(1)),
        ("body".to_owned(), CborValue::Map(vec![])),
    ]);
    let csb = cbor::encode(&raw);
    let id = iroh_rooms_v2_core::ids::ContentEventId::from_bytes(domain::blake3_domain(
        domain::CONTENT_EVENT_ID,
        &csb,
    ));
    let sig = author.sign(&domain::signing_message(domain::CONTENT_EVENT_SIGN, &csb));
    let env = Envelope {
        id,
        signed: csb,
        sig,
        signer: author.member_id(),
    };
    assert_eq!(decode_content(&env).err(), Some(Reject::UnknownContentKind));
}

#[test]
fn negative_invalid_content() {
    let author = key(0xd0);
    // message.text requires a `body` text key; an empty body map is invalid.
    let body = ContentEventBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: author.member_id(),
        kind: ContentKind::MessageText,
        version: 1,
        stream_id: None,
        body: CborValue::Map(vec![]),
    };
    let env = signed::seal(&body, &author);
    assert_eq!(decode_content(&env).err(), Some(Reject::InvalidContent));
}

#[test]
fn negative_id_mismatch() {
    let admin = admin_key();
    let mut env = signed::seal(&genesis_entry(), &admin);
    env.id = GovernanceEntryId::from_bytes([0xff; LEN]);
    assert_eq!(decode_entry(&env).err(), Some(Reject::IdMismatch));
}

#[test]
fn negative_bad_signature() {
    let admin = admin_key();
    let mut env = signed::seal(&genesis_entry(), &admin);
    // Flip one signature byte; CSB and id remain valid.
    let mut sig_bytes = *env.sig.as_bytes();
    sig_bytes[0] ^= 0x01;
    env.sig = Signature::from_bytes(sig_bytes);
    assert_eq!(decode_entry(&env).err(), Some(Reject::BadSignature));
}

#[test]
fn negative_missing_dependency() {
    // An empty fold with no room id cannot converge.
    assert_eq!(
        GovernanceFold::new().finish().err(),
        Some(Reject::MissingDependency)
    );
}

#[test]
fn negative_insufficient_authorization() {
    let outcome = folded_outcome();
    // A non-admin attempts an admin-gated AddMember.
    let imposter = key(0xee);
    let entry = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: imposter.member_id(),
        seq: 3,
        parent: Some(ADD_ENTRY_ID.parse().unwrap()),
        epoch: 1_004,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0xc0; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    assert_eq!(
        authorize_governance_entry(&outcome.state, &entry, &[]).err(),
        Some(Reject::InsufficientAuthorization)
    );
}

#[test]
fn negative_fork_detected() {
    let admin = admin_key();
    let g = genesis_entry();
    let gid = entry_id(&g);
    // Two admin entries at the same seq/parent → equivocation.
    let branch_a = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 2_001,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x01; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let branch_b = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 2_001,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x02; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let outcome = GovernanceFold::new()
        .entry(g)
        .entry(branch_a)
        .entry(branch_b)
        .finish()
        .unwrap();
    assert!(
        !outcome.unresolved_forks.is_empty(),
        "a same-seq fork must be detected"
    );
    assert!(
        outcome.items.iter().any(|item| matches!(
            item,
            iroh_rooms_v2_core::governance::FoldItem::Rejected {
                reason: Reject::ForkDetected,
                ..
            }
        )),
        "the fold must report ForkDetected"
    );
}

#[test]
fn negative_unresolved_fork() {
    let admin = admin_key();
    let g = genesis_entry();
    let gid = entry_id(&g);
    let branch_a = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 2_001,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x01; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let branch_b = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 2,
        parent: Some(gid),
        epoch: 2_001,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x02; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    let outcome = GovernanceFold::new()
        .entry(g)
        .entry(branch_a)
        .entry(branch_b)
        .finish()
        .unwrap();
    // The admin has an unresolved fork → authorization fails closed.
    let next = GovernanceEntryBody {
        schema_version: SCHEMA,
        room_id: room(),
        author: admin.member_id(),
        seq: 3,
        parent: Some(gid),
        epoch: 2_002,
        action: GovernanceAction::AddMember {
            member: MemberId::from_bytes([0x03; LEN]),
            device: DeviceId::from_bytes([0; LEN]),
            role: Role::Member,
        },
    };
    assert_eq!(
        authorize_governance_entry(&outcome.state, &next, &[]).err(),
        Some(Reject::UnresolvedFork)
    );
}

#[test]
fn negative_invalid_fork_resolution() {
    let resolver = key(0xe0);
    // Accept with a winner that is NOT one of the evidence pair.
    let body = ForkResolutionBody {
        schema_version: SCHEMA,
        room_id: room(),
        signer: resolver.member_id(),
        evidence: [
            FORK_EVIDENCE_0.parse().unwrap(),
            FORK_EVIDENCE_1.parse().unwrap(),
        ],
        action: ForkResolveAction::Accept {
            winner: GovernanceEntryId::from_bytes([0xee; LEN]),
        },
        epoch: 2_002,
    };
    let env = signed::seal(&body, &resolver);
    assert_eq!(
        decode_fork_resolution(&env).err(),
        Some(Reject::InvalidForkResolution)
    );
}

#[test]
fn negative_state_root_mismatch() {
    let admin = admin_key();
    let outcome = folded_outcome();
    let body = CheckpointBody {
        schema_version: SCHEMA,
        room_id: outcome.room_id,
        state_root: iroh_rooms_v2_core::ids::StateRoot::from_bytes([0xff; LEN]),
        member_root: outcome.member_root,
        governance_tip: None,
        unresolved_forks: Vec::new(),
        epoch: 1,
        seq: 1,
    };
    let env = signed::seal(&body, &admin);
    assert_eq!(
        validate_against_state(&env, &outcome.state).err(),
        Some(Reject::StateRootMismatch)
    );
}

#[test]
fn negative_snapshot_hash_mismatch() {
    // Reachable via the empty-`unresolved_forks`-array normalization gap: when
    // the CSB carries an explicit empty array but the body re-encode omits it
    // (CheckpointBody omits empty unresolved_forks), the recomputed snapshot
    // hash diverges from the id pinned over the original CSB. This is the only
    // public path that fires SnapshotHashMismatch today (decode_verified pins
    // the id to the exact CSB, so a plain id-swap surfaces as IdMismatch).
    let admin = admin_key();
    let outcome = folded_outcome();

    // Build the canonical body value, then inject an explicit empty
    // `unresolved_forks` array so the signed CSB differs from the re-encoded
    // body (which omits it).
    let body = CheckpointBody {
        schema_version: SCHEMA,
        room_id: outcome.room_id,
        state_root: outcome.state_root,
        member_root: outcome.member_root,
        governance_tip: None,
        unresolved_forks: Vec::new(),
        epoch: 1,
        seq: 1,
    };
    let mut value = body.to_cbor();
    // Append an explicit empty unresolved_forks array (canonical key order
    // places it after `seq`, before `governance_tip`; here we only need it
    // present so the signed bytes differ from the re-encode).
    if let CborValue::Map(ref mut entries) = value {
        entries.push(("unresolved_forks".to_owned(), CborValue::Array(vec![])));
    }
    let csb = cbor::encode(&value);
    let id = SnapshotHash::from_bytes(domain::blake3_domain(domain::SNAPSHOT_HASH, &csb));
    let sig = admin.sign(&domain::signing_message(domain::CHECKPOINT_SIGN, &csb));
    let env = Envelope {
        id,
        signed: csb,
        sig,
        signer: admin.member_id(),
    };
    assert_eq!(
        validate_against_state(&env, &outcome.state).err(),
        Some(Reject::SnapshotHashMismatch),
        "explicit empty unresolved_forks must diverge from the omitted re-encode"
    );
}

#[test]
fn negative_invalid_merkle_proof() {
    // Corrupt one sibling hash in an otherwise-valid inclusion proof.
    let mut map = iroh_rooms_v2_core::member::MerkleMap::new();
    map.insert_value(b"alice", &CborValue::Uint(1));
    let root = map.root();
    let mut proof = map.prove_inclusion(b"alice").expect("leaf is set");
    proof.siblings[0] = [0xff; LEN];
    assert_eq!(proof.verify(&root, true), Err(Reject::InvalidMerkleProof));
}

// ===========================================================================
// §12 Blocked codes — declared but not emitted by any public path (spec D7).
// These tests document the gap and must turn into real vectors when a path
// appears. They assert the codes are STILL unreachable so the gap can't close
// silently.
// ===========================================================================

#[test]
fn negative_blocked_codes_have_no_reachable_vector() {
    // Verified by `rg "Reject::(WrongDomain|InvalidApproval)" src/` — the two
    // codes below appear ONLY in error.rs (their enum definition + all_codes),
    // never at a construction site. Per spec §5 Step 6 we do not fake vectors
    // for unreachable codes; the completeness test pins this list.
    assert!(
        !code_is_constructed_in_lib("WrongDomain"),
        "WrongDomain became reachable: replace this blocked entry with a real vector"
    );
    assert!(
        !code_is_constructed_in_lib("InvalidApproval"),
        "InvalidApproval became reachable: replace this blocked entry with a real vector"
    );
}

/// Heuristic reachability check: scan the crate's source for a construction site
/// of `Reject::<Variant>` outside `error.rs`. Returns true if any exists. This is
/// intentionally conservative — it surfaces the moment a path is wired in.
fn code_is_constructed_in_lib(variant: &str) -> bool {
    // The crate source lives at `src/` relative to the test's CARGO_MANIFEST_DIR.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = std::path::Path::new(manifest).join("src");
    let needle = format!("Reject::{variant}");
    walk_and_find(&src, &needle)
}

fn walk_and_find(dir: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if walk_and_find(&path, needle) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            // Skip error.rs itself (the definition site, not a construction site).
            if path.file_name().is_some_and(|n| n == "error.rs") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text.contains(needle) {
                    return true;
                }
            }
        }
    }
    false
}

// ===========================================================================
// §13 Completeness — every Reject code has a (possibly blocked) vector, every
// positive record type is covered, and no vector names an unknown code.
// ===========================================================================

/// The full set of codes with an active or blocked vector here. Must equal
/// `error::all_codes()`.
const COVERED_CODES: &[&str] = &[
    "non_canonical_encoding",
    "unknown_version",
    "unknown_record_kind",
    "unknown_content_kind",
    "invalid_content",
    "id_mismatch",
    "bad_signature",
    "wrong_domain", // blocked
    "missing_dependency",
    "insufficient_authorization",
    "invalid_approval", // blocked
    "fork_detected",
    "unresolved_fork",
    "invalid_fork_resolution",
    "state_root_mismatch",
    "snapshot_hash_mismatch",
    "invalid_merkle_proof",
];

#[test]
fn every_reject_code_has_a_vector() {
    let all = iroh_rooms_v2_core::error::all_codes();
    for code in &all {
        assert!(
            COVERED_CODES.contains(code),
            "Reject code {code} has no golden vector (active or blocked)"
        );
    }
    for code in COVERED_CODES {
        assert!(
            all.contains(code),
            "vector names code {code} which is not in error::all_codes()"
        );
    }
    assert_eq!(
        all.len(),
        COVERED_CODES.len(),
        "code/vector count mismatch: {} codes, {} vectors",
        all.len(),
        COVERED_CODES.len()
    );
}

#[test]
fn every_positive_record_type_is_covered() {
    // Each tag maps to a `positive_*` test above. This is a static reminder: if
    // a record type is added, add both a vector and a tag here.
    for tag in POSITIVE_RECORD_TYPES {
        assert!(
            !tag.is_empty(),
            "positive record-type tag must be non-empty"
        );
    }
    assert_eq!(
        POSITIVE_RECORD_TYPES.len(),
        7,
        "expected 7 positive vectors"
    );
}

#[test]
fn blocked_codes_are_exactly_the_unreachable_set() {
    // The set of blocked codes is part of the frozen contract; if a code becomes
    // reachable, the blocked test above starts failing and this set must shrink.
    let all = iroh_rooms_v2_core::error::all_codes();
    let blocked_in_taxonomy: Vec<&str> = all
        .iter()
        .filter(|c| BLOCKED_CODES.contains(c))
        .copied()
        .collect();
    assert_eq!(
        blocked_in_taxonomy.as_slice(),
        BLOCKED_CODES,
        "blocked code set drifted"
    );
}

// ===========================================================================
// §14 Frozen-metadata fence — the fixture files carry the frozen markers and
// mirror the constants here (spec §5 Step 8).
// ===========================================================================

#[test]
fn fixture_files_carry_frozen_markers() {
    assert!(
        GOLDEN_JSON.contains("\"schema\": \"iroh-rooms-v2-golden-vectors/v2\""),
        "aggregate fixture missing schema marker"
    );
    assert!(
        GOLDEN_JSON.contains("\"frozen\": true"),
        "aggregate fixture missing frozen=true marker"
    );
    assert!(
        GOLDEN_JSON.contains("\"requires_schema_bump_on_change\": true"),
        "aggregate fixture missing requires_schema_bump_on_change marker"
    );
    assert!(
        GOLDEN_README.contains("These vectors are FROZEN"),
        "README missing frozen declaration"
    );
}

#[test]
fn fixture_json_lists_every_vector_category() {
    for tag in POSITIVE_RECORD_TYPES {
        let needle = format!("\"record_type\": \"{tag}\"");
        assert!(
            GOLDEN_JSON.contains(&needle),
            "positive fixture missing record_type {tag}"
        );
    }
    for code in COVERED_CODES {
        let needle = format!("\"expected_reject_code\": \"{code}\"");
        assert!(
            GOLDEN_JSON.contains(&needle),
            "negative fixture missing reject code {code}"
        );
    }
    for code in BLOCKED_CODES {
        let name = code.replace('_', "-");
        let needle = format!(
            "\"name\": \"{name}\", \"expected_reject_code\": \"{code}\", \"status\": \"blocked\""
        );
        assert!(
            GOLDEN_JSON.contains(&needle),
            "blocked fixture missing status=blocked for {code}"
        );
    }
}

#[test]
fn fixture_json_mirrors_rust_constants() {
    // The JSON is the human-reviewable mirror of the Rust constants above. If
    // one drifts from the other this fails, forcing a coordinated (reviewable)
    // schema bump rather than a silent edit.
    for hex_value in [
        ENTRY_CSB,
        ENTRY_ID.strip_prefix("blake3:").unwrap(),
        APPROVAL_CSB,
        APPROVAL_ID_STR.strip_prefix("blake3:").unwrap(),
        CHECKPOINT_CSB,
        CHECKPOINT_ID.strip_prefix("blake3:").unwrap(),
        LEAF_CSB,
        CONTENT_CSB,
        CONTENT_ID.strip_prefix("blake3:").unwrap(),
        FORK_CSB,
        FORK_ID.strip_prefix("blake3:").unwrap(),
        COMMUNITY_DERIVED_HASH_HEX,
        // Signatures: the frozen Ed25519 outputs are independent copies in the
        // Rust constants and the JSON `signature_hex` fields; pin them so a
        // signature cannot drift between the two without a coordinated bump.
        ENTRY_SIG,
        APPROVAL_SIG,
        CHECKPOINT_SIG,
        CONTENT_SIG,
        FORK_SIG,
        // Merkle boundary + derived roots and the community payload.
        LEAF_MERKLE_KEY,
        LEAF_VALUE_HASH,
        LEAF_HASH,
        MEMBER_ROOT.strip_prefix("blake3:").unwrap(),
        PROPOSED_STATE_ROOT.strip_prefix("blake3:").unwrap(),
        ADD_ENTRY_ID.strip_prefix("blake3:").unwrap(),
        COMMUNITY_PAYLOAD_HEX,
        FORK_EVIDENCE_0.strip_prefix("blake3:").unwrap(),
        FORK_EVIDENCE_1.strip_prefix("blake3:").unwrap(),
        // Principal public keys (signer identities).
        ADMIN_ID,
        APPROVER_ID,
        AUTHOR_ID,
        RESOLVER_ID,
    ] {
        assert!(
            GOLDEN_JSON.contains(hex_value),
            "frozen hex value {hex_value} is in the Rust constants but missing from the JSON fixture — they must mirror"
        );
    }
}

// ===========================================================================
// §15 End-to-end trust-boundary reconstruction (spec §5 Step 4.4 / 4.5).
//
// Every positive vector above builds the envelope from the *logical body* via
// `seal`. These tests instead reconstruct each envelope straight from the FROZEN
// fixture bytes — exactly the `{ id, signed, sig, signer }` a receiver pulls off
// the wire/storage — and run the canonical verifier on those independent bytes.
// This is the cross-boundary property the per-vector unit tests cannot cover: if
// `seal` and the raw-bytes verifier ever diverged, the body-built vectors would
// still pass while this fence would fail. They also prove the domain-separation
// fence has teeth: a signature valid under one record's domain does not verify
// under another's, blocking cross-record replay.
// ===========================================================================

/// Rebuild an envelope directly from frozen hex fields (the wire/storage shape),
/// independent of any logical-body builder. `sig`/`signer` parse via the field
/// types (Ed25519 `Signature` / `MemberId`).
fn envelope_from_frozen<B: SignedBody>(
    id: B::Id,
    csb_hex: &str,
    sig_hex: &str,
    signer_hex: &str,
) -> Envelope<B::Id> {
    Envelope {
        id,
        signed: hx(csb_hex),
        sig: sig_hex.parse().expect("frozen signature hex"),
        signer: signer_hex.parse().expect("frozen signer hex"),
    }
}

/// The full end-to-end round trip on independent bytes for one record family:
/// reconstruct from frozen fields → verify the exact received CSB → confirm the
/// decoded body re-encodes to the verbatim received bytes → re-seal the decoded
/// body and confirm every envelope field reproduces byte-for-byte.
fn assert_frozen_envelope_e2e<B: SignedBody>(
    id: B::Id,
    csb_hex: &str,
    sig_hex: &str,
    signer_hex: &str,
    reseal_secret: &SigningKey,
) {
    let env = envelope_from_frozen::<B>(id, csb_hex, sig_hex, signer_hex);
    let received = env.signed.clone();

    // (1) The canonical verifier accepts the exact received bytes (canonicality,
    //     id, signature, and body decode all pass on the wire/storage shape).
    let body = signed::verify_envelope::<B>(&env)
        .expect("frozen bytes must verify through the canonical verifier");

    // (2) Verbatim preservation: the decoded body re-serializes to the exact
    //     received CSB — `encode(decode(received)) == received` (spec Step 4.4).
    assert_eq!(
        signed::to_csb(&body),
        received,
        "decoded body does not re-encode to the received CSB"
    );

    // (3) Full pipeline determinism: re-sealing the decoded body reproduces id,
    //     CSB, signature, and signer byte-for-byte (the body-builder path and the
    //     frozen-bytes path converge on one envelope).
    let resealed = signed::seal(&body, reseal_secret);
    assert_eq!(resealed.id, env.id, "re-sealed id drifted from frozen");
    assert_eq!(
        resealed.signed, env.signed,
        "re-sealed CSB drifted from frozen"
    );
    assert_eq!(
        resealed.sig, env.sig,
        "re-sealed signature drifted from frozen"
    );
    assert_eq!(
        resealed.signer, env.signer,
        "re-sealed signer drifted from frozen"
    );
}

#[test]
fn e2e_every_signed_record_round_trips_from_frozen_bytes() {
    // GovernanceEntry (InitRoom) — admin seed [0xa0; 32].
    assert_frozen_envelope_e2e::<GovernanceEntryBody>(
        ENTRY_ID.parse().unwrap(),
        ENTRY_CSB,
        ENTRY_SIG,
        ADMIN_ID,
        &admin_key(),
    );
    // GovernanceApproval (AddMember) — approver seed [0xc0; 32].
    assert_frozen_envelope_e2e::<ApprovalBody>(
        APPROVAL_ID_STR.parse::<ApprovalId>().unwrap(),
        APPROVAL_CSB,
        APPROVAL_SIG,
        APPROVER_ID,
        &key(0xc0),
    );
    // GovernanceCheckpoint (clean state) — admin seed [0xa0; 32].
    assert_frozen_envelope_e2e::<CheckpointBody>(
        CHECKPOINT_ID.parse().unwrap(),
        CHECKPOINT_CSB,
        CHECKPOINT_SIG,
        ADMIN_ID,
        &admin_key(),
    );
    // ContentEvent (message.text) — author seed [0xd0; 32].
    assert_frozen_envelope_e2e::<ContentEventBody>(
        CONTENT_ID.parse().unwrap(),
        CONTENT_CSB,
        CONTENT_SIG,
        AUTHOR_ID,
        &key(0xd0),
    );
    // SignedForkResolution (Accept) — resolver seed [0xe0; 32].
    assert_frozen_envelope_e2e::<ForkResolutionBody>(
        FORK_ID.parse().unwrap(),
        FORK_CSB,
        FORK_SIG,
        RESOLVER_ID,
        &key(0xe0),
    );
}

#[test]
fn e2e_cross_domain_signature_is_not_replayable() {
    // A governance-entry signature is valid only under GOVERNANCE_ENTRY_SIGN.
    // Presenting the SAME CSB + signature to the ContentEvent verifier must fail
    // at signature verification: the signing-message prefix differs, so the
    // domain-separation fence (§9) blocks cross-record replay. The envelope id
    // is recomputed under the TARGET domain so the failure isolates to the
    // signature check rather than id-mismatch.
    let gov_csb = hx(ENTRY_CSB);
    let content_id =
        ContentEventId::from_bytes(domain::blake3_domain(domain::CONTENT_EVENT_ID, &gov_csb));
    let gov_as_content = Envelope {
        id: content_id,
        signed: gov_csb,
        sig: ENTRY_SIG.parse().expect("frozen signature"),
        signer: ADMIN_ID.parse().expect("frozen signer"),
    };
    assert_eq!(
        signed::verify_envelope::<ContentEventBody>(&gov_as_content).err(),
        Some(Reject::BadSignature),
        "a governance signature must not verify as a content signature"
    );

    // Symmetric direction: a content signature must not verify as a governance
    // signature. Together the two cases prove the fence is pairwise, not a fluke
    // of one record family.
    let content_csb = hx(CONTENT_CSB);
    let gov_id = GovernanceEntryId::from_bytes(domain::blake3_domain(
        domain::GOVERNANCE_ENTRY_ID,
        &content_csb,
    ));
    let content_as_gov = Envelope {
        id: gov_id,
        signed: content_csb,
        sig: CONTENT_SIG.parse().expect("frozen signature"),
        signer: AUTHOR_ID.parse().expect("frozen signer"),
    };
    assert_eq!(
        signed::verify_envelope::<GovernanceEntryBody>(&content_as_gov).err(),
        Some(Reject::BadSignature),
        "a content signature must not verify as a governance signature"
    );
}
