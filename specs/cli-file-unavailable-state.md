# Spec: File unavailable state in the CLI (`iroh-rooms file fetch` honest availability)

| | |
|---|---|
| **Issue** | #30 — [IR-0205] Implement file unavailable state in CLI |
| **Parent** | #3 |
| **Labels** | type/feature, area/cli, area/blob, area/dx, priority/p1, risk/low |
| **Dependencies** | #29 — [IR-0204] File fetch + serve plane (**landed**: `iroh-rooms file fetch`, `net::blob::fetch::FetchOutcome`). Builds on #25 — [IR-0110] CLI error taxonomy (**landed**: `ErrorCode`, `error[<code>]:` render contract, reserved `blob_unavailable`). |
| **Traceability** | `PRD.v0.3.md` §9.2 (MVP file-flow + MVP limitation: "If no peer with the file is online, the file may not be fetchable … acceptable for MVP if the CLI reports the state clearly"), §14 (Availability Model — "no cloud inbox", "no guaranteed offline delivery"), §16 (CLI Requirements — UX req 3: "distinguish offline peer, unauthorized peer, unavailable blob, invalid ticket, invalid signature"; UX req 4: "Availability limitations should be explicit, not hidden"; UX req 5: script-friendly). |
| **Status** | Planning — spec only. No production code changed by this document. |
| **Type** | Feature / DX (honest failure-state reporting; a thin classification + taxonomy-adoption layer over the landed fetch). |

---

## 1. Summary

`iroh-rooms file fetch` (landed in IR-0204 / #29) already retrieves a blob from an
authorized provider, verifies BLAKE3-256, and saves it. What it does **not** yet do is
report its *failure* states honestly and distinctly. Today every fetch failure path is
a **plain, uncoded `anyhow::bail!`**, so:

1. **The reserved `blob_unavailable` code is never emitted.** IR-0110 (#25) defined
   `ErrorCode::BlobUnavailable` (code `blob_unavailable`, category Connectivity, exit
   `6`) but left it `#[allow(dead_code)]` — *"Not yet constructed anywhere; `file fetch`
   is explicitly out of scope"* — and its `BLOB_UNAVAILABLE_MESSAGE` still reads *"peer
   fetch is not implemented in this build"*, which is now **stale** (#29 shipped fetch).
   A "no provider online" fetch therefore prints a bare `error: file … is currently
   unavailable …` and exits `1` (generic) — a script sees an *unclassified* failure, so
   the availability limitation is effectively **hidden** behind a generic error
   (violating PRD §16 UX req 4 and issue AC1).
2. **Unauthorized and unavailable are conflated.** The per-provider loop prints a
   per-provider diagnostic (`denied the connection` / `will not serve this hash` /
   `unreachable`) but then, when no provider served the bytes, **unconditionally** bails
   with *"currently unavailable: no provider holding it is online"* — regardless of
   whether every provider actually **refused the connection** (`DeniedAtConnect` — an
   *authorization* wall) or was simply **offline** (`Unavailable` — an *availability*
   gap). Issue AC2 requires these be distinct.
3. **The integrity ("invalid hash") failure is uncoded.** A `HashMismatch` outcome hard-
   fails with a clear message but exits `1` (generic), so a script cannot distinguish a
   corrupt/lying reference from any other failure. The issue Test Plan ("hash mismatch")
   and Scope ("distinguish … invalid hash") require it be a distinct, coded state.
4. **The fetcher-not-active pre-check is uncoded.** `file share` codes the same check as
   `not_a_member` (exit 3); `file fetch` uses a plain `bail!` (exit 1). The Test Plan's
   "non-member" case wants a distinct, script-branchable authorization failure.

This issue is **CLI-only and additive**. It classifies the terminal fetch outcome into
three distinct, script-branchable, PRD-aligned states — **unavailable**, **unauthorized**,
**invalid hash** — each rendered through the already-landed IR-0110 `error[<code>]:`
contract, un-reserves `blob_unavailable`, adds the one CLI-native integrity code the
taxonomy still lacks, and reconciles the docs. **No protocol, event schema, network,
serve/fetch, or authorization behaviour changes.** The bytes-moving mechanism (`net::blob`)
is untouched; only how the CLI *names* the result changes.

This document is detailed enough to execute without re-deriving scope.

---

## 2. Background & current repository state

**Read before starting:**

- `crates/iroh-rooms-cli/src/file.rs` — the `fetch(...)` orchestrator (IR-0204). The
  per-provider loop (`for provider_addr in &providers { … node.fetch_file(…) … }`) and
  its four terminal `bail!`s are the exact code this issue re-points. The
  `resolve_providers`, `sanitize_name`, `save_atomic`, `parse_file_id` helpers and the
  `FetchSummary`/`print_fetch` success surface are **unchanged**.
- `crates/iroh-rooms-cli/src/error.rs` — the IR-0110 taxonomy: `ErrorCode` (incl. the
  reserved `BlobUnavailable` and `BLOB_UNAVAILABLE_MESSAGE`, both `#[allow(dead_code)]`),
  `ErrorCategory` (`Connectivity` → exit `6`, `Auth` → `3`, `Integrity` → `4`),
  `CliError`, `CodedResultExt::coded`, `bail_coded!`, `code_of`. This is the framework;
  this issue *emits* codes through it.
- `crates/iroh-rooms-cli/src/main.rs` — the single render point: a coded failure prints
  `error[<code>]: {err:#}` and exits with the category code; an uncoded one prints
  `error: {err:#}` and exits `1`. **No change needed here** — this issue just makes the
  fetch paths coded so they flow through the first arm.
- `crates/iroh-rooms-net/src/blob/fetch.rs` (or `blob.rs`) — `FetchOutcome { Fetched,
  DeniedAtConnect, DeniedPerHash, HashMismatch, Unavailable }` and `Node::fetch_file`.
  **Read-only for this issue** — the outcome enum is exactly the classification input.
- `PRD.v0.3.md` §9.2, §14, §16; `docs/getting-started.md` "Unavailable file"
  troubleshooting case + the "Stable error/warning lines and exit codes" section (which
  **already lists `blob_unavailable`** as a rendered code — a doc-vs-behaviour gap this
  issue closes).

**Current behaviour of each fetch failure path (all in `file.rs::fetch`):**

| Condition | Current code | Current exit | Problem |
|---|---|---|---|
| Caller not an active member (pre-check) | plain `bail!` | 1 (generic) | should be `not_a_member` / exit 3 (Test Plan "non-member") |
| Bad `file_id` arg | plain `bail!` | 1 | should be `invalid_argument` / exit 2 |
| Reference not found after sync wait | plain `bail!` | 1 | should be coded (`no_such_file`, exit 2) |
| Unsupported `blob_format` | plain `bail!` | 1 | should be coded (`invalid_argument`, exit 2) |
| `providers` empty after self-skip | plain `bail!` "currently unavailable" | 1 | should be `blob_unavailable` / exit 6 (AC1) |
| Loop exhausted, none served | plain `bail!` "currently unavailable" | 1 | **conflates** unauthorized vs unavailable (AC2); should split into `peer_unauthorized`/exit 3 vs `blob_unavailable`/exit 6 |
| `HashMismatch` | plain `bail!` "integrity check FAILED" | 1 | should be a distinct integrity code / exit 4 (AC "invalid hash") |

**Landed facts this issue relies on (do not re-derive):**

- Membership is fold-derived; `snapshot.is_active(&id)` is the active-member predicate
  (`share` already uses it with a coded failure).
- `FetchOutcome` is the authoritative per-attempt classification; `Node::fetch_file`
  is bounded by the `--timeout` (default `30s`, `DEFAULT_FETCH_TIMEOUT`), so an offline
  provider yields `Unavailable`, never a hang (AC/PRD §18.2 already satisfied at the
  mechanism layer).
- The CLI installs **no `tracing` subscriber** (project memory *CLI has no tracing
  subscriber*): the per-provider diagnostics already go to **stderr** via `eprintln!`,
  and the terminal error goes to stderr via `main.rs`. stdout stays clean (the success
  `FetchSummary` lines only). This issue preserves that split.
- Workspace lints are strict (`unsafe_code = "forbid"`, clippy `all` + `pedantic`);
  `scripts/verify.sh` (fmt `--check`, clippy `-D warnings`, `--all-features` tests) is
  the real CI gate (project memory *verify.sh is the real CI gate*).

---

## 3. Goal, scope, and non-goals

### 3.1 Goal

Represent an unavailable file **honestly** — the CLI names the *unavailable*,
*unauthorized*, and *invalid-hash* fetch-failure states distinctly and script-friendly,
with PRD-aligned availability language and actionable retry guidance, instead of
collapsing them into one generic error.

### 3.2 In scope (this issue)

1. **Un-reserve `blob_unavailable`.** Remove the `#[allow(dead_code)]` from
   `ErrorCode::BlobUnavailable`, and correct/replace the stale
   `BLOB_UNAVAILABLE_MESSAGE` (peer fetch **is** implemented now).
2. **Emit `blob_unavailable` (exit 6)** on the honest "no online provider holds this
   file" terminal state (empty provider set after self-skip, or a loop where every
   attempt was `Unavailable`/`DeniedPerHash`), with PRD §14 availability language and a
   retry hint (AC1).
3. **Distinguish unauthorized from unavailable (AC2):**
   - The **fetcher-not-active pre-check** → `ErrorCode::Reject(RejectReason::NotAMember)`
     (`not_a_member`, exit 3), matching `file share`. (Test Plan "non-member".)
   - The **aggregate loop** where every reachable provider *refused the connection*
     (`DeniedAtConnect`) → `ErrorCode::PeerUnauthorized` (`peer_unauthorized`, exit 3),
     never "unavailable".
4. **Distinguish invalid hash (AC "invalid hash" / Test Plan "hash mismatch"):** the
   `HashMismatch` hard-fail → a distinct **integrity** code (exit 4). Add the one CLI-
   native code the taxonomy lacks (see §5.4 / OQ-1).
5. **Code the ancillary fetch paths** for a uniform surface: bad `file_id` →
   `invalid_argument` (exit 2); reference-not-found → `no_such_file` (exit 2);
   unsupported `blob_format` → `invalid_argument` (exit 2).
6. **Clear retry guidance** in each message (what to do next), per PRD §16 UX req 1.
7. **Docs + conformance:** reconcile `README.md` "Error codes" table (already lists
   `blob_unavailable`), `docs/getting-started.md` "Unavailable file" case (render the
   coded line + exit code; drop the stale "file fetch not yet implemented" doc-status
   caveat that IR-0204 left behind), and extend the docs-conformance gate so
   `blob_unavailable` (and the new integrity code) are proven **emitted**, not just
   reserved.
8. **Tests:** CLI tests for the three Test-Plan scenarios (provider offline → unavailable;
   non-member → unauthorized; hash mismatch → invalid hash), each asserting the distinct
   `error[<code>]:` line **and** exit code; plus a fast unit test for the aggregate
   classifier.

### 3.3 Out of scope / non-goals (explicit)

- **No change to `net::blob` (serve or fetch), `FetchOutcome`, `Node::fetch_file`, the
  event schema, the ACL, or any authorization decision.** This issue is a naming/render
  layer; the bytes-moving and gating logic is IR-0204's and stays byte-identical.
- **No new availability guarantee.** No queue, no retry loop, no always-on/archive
  provider, no cloud inbox (PRD §14). "Unavailable" stays honest, not aspirational.
- **No `--json` error envelope.** The stable surface remains the `error[<code>]:` stderr
  line + the category exit code (IR-0110 §3.3 non-goal, unchanged).
- **No retro-coding of the whole CLI long tail.** Only the `file fetch` failure surface
  (the six paths in §2's table) is converted here.
- **No change to the fetch success surface** (`FetchSummary` / `print_fetch`) or to
  `file share` / `file list`.
- **No `hash_seq` support** (still rejected as unsupported format — IR-0204 non-goal).

---

## 4. Placement & dependencies

### 4.1 Where the code lives

| Change | Crate / file | Kind |
|---|---|---|
| Un-reserve `BlobUnavailable`; fix reserved message | `iroh-rooms-cli/src/error.rs` | edit (remove `#[allow(dead_code)]`) |
| (If chosen) new CLI-native integrity code for hash mismatch | `iroh-rooms-cli/src/error.rs` | additive variant |
| Aggregate outcome classifier + coded terminal `bail`s | `iroh-rooms-cli/src/file.rs` (`fetch`) | edits (no new command) |
| Fetch CLI failure-state tests | `iroh-rooms-cli/tests/file_cli.rs` | additive tests |
| Aggregate-classifier unit test | `iroh-rooms-cli/src/file.rs` (`#[cfg(test)]`) | additive tests |
| Error-code table (`blob_unavailable` emitted; new integrity code) | `README.md` | docs |
| "Unavailable file" troubleshooting + stale doc-status caveat | `docs/getting-started.md` | docs |
| Docs-conformance: reserved → emitted | `iroh-rooms-cli/tests/docs_conformance.rs` | edit |

`error.rs` gains no new external dependency; the classifier reads the existing
`iroh_rooms_net::FetchOutcome` (already a CLI dep). **No net/core crate change.**

### 4.2 Dependency boundary

- **IR-0204 (#29)** provides the mechanism (`FetchOutcome`, `Node::fetch_file`, the
  serve plane). This issue consumes its outcomes; it does not modify it.
- **IR-0110 (#25)** provides the taxonomy + render contract. This issue is exactly the
  "swap the reserved stub for real emission with zero taxonomy change" handoff IR-0110
  §4.2/§5.10 described — plus the one integrity code IR-0110 did not anticipate for a
  fetched-blob content mismatch (§5.4).

---

## 5. Design

### 5.1 The three honest terminal states (AC1 + AC2 + invalid-hash)

The fetch has exactly one success and three distinct failure *classes* the user must be
able to tell apart. Mapping the issue Scope + Test Plan:

| State | Trigger | Code | Category | Exit | PRD |
|---|---|---|---|---|---|
| **Unavailable** | no online provider holds the blob (empty provider set, or every attempt `Unavailable`/`DeniedPerHash`) | `blob_unavailable` | Connectivity | 6 | §9.2 limitation, §14 |
| **Unauthorized** | caller not active (pre-check), **or** every reachable provider `DeniedAtConnect` | `not_a_member` (pre-check) / `peer_unauthorized` (aggregate) | Auth | 3 | §16 UX req 3 |
| **Invalid hash** | assembled bytes' BLAKE3 ≠ declared (`HashMismatch`) | `hash_mismatch` (new, §5.4) | Integrity | 4 | §9.2 step 6 |

Plus the ancillary usage-class paths (bad arg / unknown file / unsupported format) →
exit 2, which round out a uniform surface but are not the three headline states.

### 5.2 Aggregate classification of the per-provider loop (the core of AC2)

The loop tries providers in order (§5.5 of IR-0204). Today it only remembers whether it
`Fetched`. This issue adds a small **tally** of the non-fetch outcomes so the terminal
decision is honest. Introduce a private helper in `file.rs`:

```rust
/// Tally of per-provider fetch outcomes, used to classify the terminal failure
/// honestly when no provider served the bytes (spec §5.2 — the AC2 unauthorized-
/// vs-unavailable split). Not a trust input; purely for reporting.
#[derive(Default)]
struct FetchTally {
    denied_at_connect: usize, // provider reachable but refused the connection (authz wall)
    denied_per_hash: usize,   // provider reachable, active, but not serving this hash
    unreachable: usize,       // provider offline / timed out (availability gap)
    attempted: usize,
}

/// The honest terminal classification when no provider served the bytes.
enum FetchFailure {
    /// Every reachable provider refused the connection — an authorization wall,
    /// not an availability gap. → peer_unauthorized (exit 3).
    Unauthorized,
    /// At least one provider was unreachable or reachable-but-not-serving, and none
    /// authorized+served. → blob_unavailable (exit 6). The honest MVP-limitation state.
    Unavailable,
}

impl FetchTally {
    fn classify(&self) -> FetchFailure {
        // Unauthorized ONLY when every attempt was a connection refusal and nothing
        // was merely unreachable or per-hash-denied. Any reachability/serving gap in
        // the mix makes the honest headline "unavailable" (a reachable-but-refusing
        // wall is the only case that is purely an authorization problem).
        if self.attempted > 0
            && self.denied_at_connect == self.attempted
        {
            FetchFailure::Unauthorized
        } else {
            FetchFailure::Unavailable
        }
    }
}
```

**Decision rule (recommended; OQ-2 records the alternative).** `Unauthorized` iff **all**
attempted providers returned `DeniedAtConnect`. Rationale: `DeniedAtConnect` is the one
outcome where the provider was reachable and actively refused *you* — a pure
authorization signal. `DeniedPerHash` (provider online, active, but hasn't synced the
`file.shared` / doesn't hold the hash) and `Unavailable` (offline) are both *availability*
gaps. If even one attempt was an availability gap, the honest headline is "unavailable"
(you may still get the file when a holder comes online), so we do not over-claim
"unauthorized". An empty provider set (all candidates were self) is `Unavailable`.

### 5.3 Rewritten terminal branches (illustrative)

The loop body records the tally alongside the existing per-provider `eprintln!`
diagnostics (kept verbatim — they are already stderr and already stable):

```rust
FetchOutcome::DeniedAtConnect => { tally.denied_at_connect += 1; eprintln!(/* unchanged */); }
FetchOutcome::DeniedPerHash   => { tally.denied_per_hash   += 1; eprintln!(/* unchanged */); }
FetchOutcome::Unavailable     => { tally.unreachable       += 1; eprintln!(/* unchanged */); }
```

Terminal decisions (after `node.shutdown()`), each **coded**:

```rust
// (a) HashMismatch — hard stop, distinct integrity failure (unchanged detection).
if let Some(got) = hash_mismatch {
    bail_coded!(
        ErrorCode::HashMismatch, // §5.4
        "integrity check FAILED: fetched bytes hash blake3:{got} but the reference \
         declares {}; refusing to save (the file reference or a provider may be \
         corrupt — do not trust this file)",
        shared.blob_hash
    );
}

// (b) No bytes served — classify unauthorized vs unavailable (AC2).
let Some((data, provider_id)) = fetched else {
    match tally.classify() {
        FetchFailure::Unauthorized => bail_coded!(
            ErrorCode::PeerUnauthorized,
            "file {file_id_str} could not be fetched: every provider refused the \
             connection — this identity ({self_id}) is not an active member from their \
             view. Ask the admin to confirm your membership has synced, then retry"
        ),
        FetchFailure::Unavailable => bail_coded!(
            ErrorCode::BlobUnavailable,
            "file {file_id_str} is currently unavailable: no peer holding it is online. \
             There is no central inbox and no guaranteed offline delivery — ask a \
             provider to run `iroh-rooms room tail {room_id}`, then retry `file fetch`"
        ),
    }
};
```

The empty-provider-set early return (before the loop) also becomes `bail_coded!(
ErrorCode::BlobUnavailable, …)` with the same §14-aligned wording.

The **availability language** (`no central inbox`, `no guaranteed offline delivery`,
`availability follows the providers`) is drawn verbatim-in-spirit from PRD §14 (AC4).

### 5.4 The invalid-hash ("integrity") code (OQ-1)

The taxonomy has **no CLI-native integrity code** — every `Integrity`-category code
today is a wrapped `RejectReason` (a *stateless event* rejection), and a fetched-blob
content mismatch is not an event rejection. Two options:

- **(Recommended) Add `ErrorCode::HashMismatch`** → `code() == "hash_mismatch"`,
  `category() == ErrorCategory::Integrity` (exit 4). Clean, script-distinct, and honest:
  a script branching on `hash_mismatch` knows the transfer's content failed verification,
  not that an event was malformed. Extend the `error.rs` `code()`/`category()` matches
  and the pinned stability tests (`codes_are_stable`, the category test).
- **(Alternative) Reuse `ErrorCode::Reject(RejectReason::InvalidContent)`** →
  `invalid_content`, exit 4. Avoids a new variant but overloads a code that elsewhere
  means "a malformed event was rejected by the stateless validator", which is misleading
  in a `file fetch` context.

Recommend the new `HashMismatch` variant (§13 OQ-1). It is the one genuinely new
taxonomy entry; it must be added to the README error table and the docs-conformance gate.

### 5.5 Un-reserving `blob_unavailable` (error.rs)

- Remove `#[allow(dead_code)]` from `ErrorCode::BlobUnavailable` and from
  `BLOB_UNAVAILABLE_MESSAGE` (now that `file fetch` constructs the code, both are live).
- **Replace the stale message.** `BLOB_UNAVAILABLE_MESSAGE` currently says *"peer fetch
  is not implemented in this build"* — factually wrong post-#29. Either (a) delete the
  constant and inline the §5.3 message at the emission site (the message is now context-
  specific: it names the file id and room), or (b) repurpose it as a short shared prefix.
  Recommend **(a)**: the honest message needs the `file_id`/`room_id` interpolation, so a
  fixed constant no longer fits; delete it and its now-obsolete unit test assertion, and
  add the real emission test in `file_cli.rs`. (Confirm no other caller references the
  constant — grep shows only the reserved-placeholder unit test in `error.rs`.)

### 5.6 Ancillary paths (uniform surface, exit 2)

- Bad `file_id` (`parse_file_id`) → `.coded(ErrorCode::InvalidArgument)` (or
  `bail_coded!`). Currently `bail!`.
- Reference-not-found after the bounded sync wait → `bail_coded!(ErrorCode::NoSuchFile,
  …)` — keep the existing actionable "has it been shared and synced? try `room tail`"
  hint.
- Unsupported `blob_format` → `bail_coded!(ErrorCode::InvalidArgument, …)` — keep the
  "raw only" message. (It is a build limitation, not an availability gap, so **not**
  `blob_unavailable`; see OQ-3.)

These are consistency wins, not headline ACs; they keep the whole `file fetch` surface
coded so no path renders the generic `error:`/exit 1.

### 5.7 What stays exactly the same

- The success path (`FetchSummary`, `print_fetch`, `saved:`/`verified:`/`size:`/`provider:`
  on stdout).
- The per-provider stderr diagnostics inside the loop (already stable).
- `HashMismatch` remaining a **hard stop** (never falls through to another provider) —
  only its *rendering* becomes coded.
- The bounded `--timeout` behaviour (no hang) — already satisfied by IR-0204.
- All of `net::blob`, the serve plane, the ACL, and every authorization verdict.

---

## 6. Implementation steps

Ordered so each step compiles and is independently testable.

### Step 0 — Confirm the outcome surface (no recon; verification)
Re-read `FetchOutcome` and confirm the four non-`Fetched` variants are exactly
`{DeniedAtConnect, DeniedPerHash, HashMismatch, Unavailable}` as consumed in
`file.rs::fetch` today. Confirm `main.rs` renders coded errors via `code_of` (it does).
No change; this de-risks the classifier match being exhaustive.

### Step 1 — Error module: un-reserve + add the integrity code
- `error.rs`: remove `#[allow(dead_code)]` from `BlobUnavailable`; add
  `ErrorCode::HashMismatch` (→ `"hash_mismatch"`, `ErrorCategory::Integrity`); extend
  `code()`, `category()`, and the `#[non_exhaustive]`-safe matches.
- Delete the stale `BLOB_UNAVAILABLE_MESSAGE` (and its reserved-placeholder unit test) or
  repurpose it (§5.5); update `codes_are_stable` and the category tests to pin
  `hash_mismatch` → Integrity → exit 4.

### Step 2 — CLI: code the pre-check + ancillary paths
- `file.rs::fetch`: the not-active pre-check → `bail_coded!(ErrorCode::Reject(
  RejectReason::NotAMember), …)` (mirror `share`). Bad `file_id`, unknown-file, and
  unsupported-format bails → their §5.6 codes.

### Step 3 — CLI: the aggregate classifier
- Add `FetchTally` + `FetchFailure` + `classify()` (§5.2). Thread the tally through the
  loop's three non-fetch arms (keep the existing `eprintln!` diagnostics). Rewrite the
  two terminal `bail!`s (empty providers; loop exhausted) and the hash-mismatch `bail!`
  per §5.3, all coded. Unit-test `classify()` (see §8).

### Step 4 — Docs + conformance
- `README.md`: in the Error codes table, **drop the `(reserved for the serve/fetch
  follow-up)` qualifier** on the `blob_unavailable` row (currently `README.md:672`, the
  exit-`6` Connectivity row) — it is now emitted by `file fetch`. Add a `hash_mismatch`
  row under Integrity (exit 4). (`peer_unauthorized` / `not_a_member` are already in the
  exit-`3` Auth row at `README.md:669`; no new row needed for the unauthorized split.)
- `docs/getting-started.md`: update the "Unavailable file" case (`docs/getting-started.md:945`)
  to show the coded line `error[blob_unavailable]: file file_…2c is currently unavailable:
  no peer holding it is online …` and exit 6; add a companion note for the `hash_mismatch`
  (exit 4) and the all-refused `peer_unauthorized` (exit 3) cases. Remove the stale
  doc-status caveat at `docs/getting-started.md:67-70` ("`file fetch` is **not yet
  implemented** … remains *illustrative*") that IR-0204 left behind, and reconcile the
  illustrative `file fetch` block to the shipped output. (The exit-code list at
  `docs/getting-started.md:986` already names `blob_unavailable`; leave it.)
- `tests/docs_conformance.rs` — **two concrete artifacts** (both currently pin the
  reserved state and will fail the moment the README/behaviour change, so update them in
  the same commit):
  1. **`ALL_ERROR_CODES`** (the pinned array; `blob_unavailable` is its last entry,
     `docs_conformance.rs:970`): add `"hash_mismatch"` under the Integrity (exit 4)
     group. `readme_documents_every_error_code` then proves the new code has a README row.
  2. **`readme_marks_blob_unavailable_as_reserved`** (`docs_conformance.rs:1050`): this
     test asserts the README contains `blob_unavailable` **and** the word `reserved`.
     Once the README drops the "reserved" qualifier this assertion breaks — **invert or
     retire it** (e.g. rename to `readme_documents_blob_unavailable_as_emitted`, asserting
     the row exists and no longer says "reserved"), so the docs gate proves the code is
     now emitted, not merely reserved. Keep the "no orphan / no undocumented code"
     invariant green.

### Step 5 — Tests
- Extend `tests/file_cli.rs` per §8 (three headline scenarios + ancillary). Reuse the
  existing per-test `IROH_ROOMS_HOME` + `assert_cmd` harness and (for the online-ish
  cases) the `--loopback` pattern / `#[ignore]` gating already used by the fetch tier.

### Step 6 — Gate
- Run `scripts/verify.sh` (fmt `--check`, clippy `-D warnings` pedantic, `--all-features`
  tests). Confirm no `#[allow(dead_code)]` remains on the now-live code path and no new
  clippy `#[allow]` creep.

---

## 7. Error model & observability

- **One render point (unchanged).** `main.rs` already renders `error[<code>]:` + category
  exit for coded failures; this issue only makes the fetch failures coded. No new
  `eprintln!` for terminal errors.
- **stdout stays clean.** Success `FetchSummary` on stdout; every failure line and every
  per-provider diagnostic on stderr (unchanged). A script does `iroh-rooms file fetch …
  2>/dev/null` for the saved path and branches on `$?` / the `error[<code>]:` line.
- **The three headline codes are script-branchable and distinct:**
  `blob_unavailable` (exit 6) ≠ `peer_unauthorized`/`not_a_member` (exit 3) ≠
  `hash_mismatch` (exit 4). This is the AC2 "distinct states" contract at the process
  boundary.
- **Availability language (AC4).** The `blob_unavailable` message states the honest MVP
  limitation in PRD §14 terms — no central inbox, no guaranteed offline delivery,
  availability follows the providers — and gives the concrete retry (`room tail` on a
  holder). No message implies a queue or eventual delivery.
- **No secrets.** The fetch path reads only the device signing key (to authenticate the
  QUIC connection) and prints none; the new messages interpolate only the public
  `file_id`, `room_id`, and the caller's `identity_id` (all already printed elsewhere).

---

## 8. Test strategy

Maps the issue **Test Plan** ("CLI tests with provider offline, non-member, and hash
mismatch") + the four ACs. In `crates/iroh-rooms-cli/tests/file_cli.rs` (via `assert_cmd`,
per-test `IROH_ROOMS_HOME`) and a `file.rs` unit test:

**Unit (fast, deterministic — the AC2 classifier):**
1. `FetchTally::classify` — all-`DeniedAtConnect` (attempted ≥ 1) → `Unauthorized`;
   all-`Unavailable` → `Unavailable`; mixed (one `DeniedAtConnect` + one `Unavailable`)
   → `Unavailable`; all-`DeniedPerHash` → `Unavailable`; zero-attempted → `Unavailable`.
2. `error.rs`: `hash_mismatch` → Integrity → exit 4 pinned; `blob_unavailable` still →
   Connectivity → exit 6; `peer_unauthorized` → Auth → exit 3 (already pinned — assert it
   survives).

**CLI integration — one assertion of (exit code, `error[<code>]:` line, message
substring) per Test-Plan scenario:**
3. **Non-member (unauthorized, offline/deterministic):** in a room where the caller is
   **not** an active member, `file fetch <room> <known-file-id>` → stderr
   `error[not_a_member]: …only an active member can fetch files…`, **exit 3**. (Pure
   local fold; no node needed — the pre-check fires first.)
4. **Provider offline (unavailable):** the caller **is** active and a `file.shared`
   reference exists locally, but no provider is reachable (no `--peer`, `--loopback`,
   short `--timeout`, provider process not running) → stderr `error[blob_unavailable]:
   file … is currently unavailable: no peer holding it is online …`, **exit 6**, nothing
   written to the downloads dir. Bounded by `--timeout` (no hang).
5. **Hash mismatch (invalid hash):** the deterministic path — drive a fetch whose
   assembled bytes' hash ≠ declared. Reuse the IR-0204 `fetch_hash`/`declared_hash` split
   at the Node layer if a CLI-level trigger is impractical; otherwise seed a `file.shared`
   whose `blob_hash` differs from the served blob and run the gated online tier →
   `error[hash_mismatch]: integrity check FAILED …`, **exit 4**, nothing saved. (`#[ignore]`
   gate if it needs two live processes, mirroring the IR-0204/IR-0109 tier.)
6. **Aggregate all-refused (unauthorized, distinct from #4):** an active caller whose
   every discovered provider refuses at connect (e.g. a provider that removed the caller,
   or an ACL that has not synced the caller in) → `error[peer_unauthorized]: …every
   provider refused the connection…`, **exit 3** — proving the AC2 split against #4's
   `blob_unavailable`. (Gated online tier; may be `#[ignore]`.)

**Ancillary (fast):**
7. Bad `file_id` → `error[invalid_argument]`, exit 2. Unknown file id in a known room
   (no reference, no `--peer`) → `error[no_such_file]`, exit 2. Unsupported `blob_format`
   (seed a `file.shared` with `blob_format="hash_seq"`) → `error[invalid_argument]`,
   exit 2 (or the OQ-3 choice), nothing written.

**Docs conformance:** `docs_conformance.rs` — `blob_unavailable` and `hash_mismatch`
appear in the README table **and** are produced by a real path (no orphan, no
undocumented code).

CI-tier tests are deterministic/offline where possible (#3, #7, unit); the online splits
(#4 unavailable, #5 hash-mismatch, #6 all-refused) use the loopback / `#[ignore]` tier.
`scripts/verify.sh` is the gate.

---

## 9. Security, privacy, reliability, performance

- **No authorization change.** The provider's two-gate ACL and every fold verdict are
  IR-0204's and untouched. This issue only *reports* the outcome; it grants nothing.
  `DeniedAtConnect`/`DeniedPerHash` remain the provider's decision — the CLI never
  overrides or infers around them.
- **Honest availability (the point of the issue).** An unauthorized wall is never
  disguised as "unavailable" and vice versa (AC2); an offline provider is reported as an
  availability limitation, not a bug (PRD §16 UX req 4, §9.2, §14). The classifier is
  **not a trust input** — it only chooses the message/code.
- **Reliability.** No new network behaviour; the bounded `--timeout` (IR-0204) still
  guarantees no hang. The classifier is O(providers) counting — off the hot path.
- **Privacy / secrets.** Messages interpolate only public identifiers already printed
  elsewhere (`file_id`, `room_id`, caller `identity_id`); no key/secret bytes on any
  stream. No new file write; the "unavailable"/"unauthorized"/"invalid hash" paths all
  write **nothing** to disk (unchanged).
- **Migration / back-compat.** No schema, wire, or DB change. The one output-format
  change is the failing `file fetch` line gaining an `error[<code>]:` prefix and a
  category exit code (was `error:`/exit 1). Scripts that only checked "non-zero = failure"
  are unaffected; scripts branching on the specific code/exit gain determinism. Documented
  in the PR and gated by docs-conformance.

---

## 10. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | Mixed-outcome loop (some denied-connect, some unreachable) mislabels the state, confusing AC2. | Med | Med | Explicit, tested `classify()` rule: `Unauthorized` **only** when *all* attempts were `DeniedAtConnect`; any availability gap ⇒ `Unavailable` (§5.2). Unit test #1 pins the mixed case. |
| R2 | Adding `ErrorCode::HashMismatch` drifts the taxonomy or the docs gate. | Low | Med | It is the only new code; add it to the pinned `code()`/`category()` tests **and** the README table + docs-conformance in the same step (Step 1 + Step 4). `#[non_exhaustive]` unchanged. |
| R3 | Deleting `BLOB_UNAVAILABLE_MESSAGE` breaks a caller/test. | Low | Low | Grep confirms the only reference is the reserved-placeholder unit test in `error.rs`; update/remove it in the same commit. |
| R4 | The hash-mismatch CLI test is hard to trigger deterministically (content-addressing). | Med | Low | Reuse IR-0204's `fetch_hash`/`declared_hash` split at the Node layer, or seed a `file.shared` whose declared hash ≠ served bytes; `#[ignore]`-gate if it needs live processes (test #5). |
| R5 | `DeniedPerHash` (provider online, reference unsynced) surprises a user as "unavailable". | Med | Low | It **is** an availability gap for MVP (the bytes are not obtainable right now); the message's retry hint ("ask a provider to run `room tail`, then retry") covers the transient case. Documented in getting-started. |
| R6 | Over-reach: coding the whole CLI tail creeps scope. | Low | Low | Scope fixed to the `file fetch` surface (§3.2); the long tail stays as-is (IR-0110 already established the incremental-adoption norm). |

---

## 11. Acceptance criteria

Maps issue #30 ACs + Test Plan.

- [ ] **AC1 — Unavailable, not generic.** With no provider online, `file fetch` fails
  with `error[blob_unavailable]: …` and exit `6` (Connectivity), not a bare `error:`/exit
  `1`. (Test #4.)
- [ ] **AC2 — Unauthorized and unavailable are distinct.** A non-member caller →
  `error[not_a_member]` exit 3 (test #3); every-provider-refused → `error[peer_unauthorized]`
  exit 3 (test #6); no-online-provider → `error[blob_unavailable]` exit 6 (test #4).
  Distinct codes **and** distinct exit codes; a script can branch on either. (Tests #1,
  #3, #4, #6.)
- [ ] **AC — Invalid hash distinct.** A `HashMismatch` → `error[hash_mismatch]` exit 4
  (Integrity), distinct from unavailable/unauthorized; nothing is saved. (Test #5.)
- [ ] **AC3 — Script-friendly.** Every fetch failure renders exactly one pinned
  `error[<code>]:` line on stderr with a documented category exit code; stdout stays
  clean; the codes are in the README table and gated by docs-conformance. (Tests #2, #7,
  docs-conformance.)
- [ ] **AC4 — PRD availability language.** The `blob_unavailable` message states the
  honest MVP limitation in PRD §14 terms (no central inbox, no guaranteed offline
  delivery; availability follows the providers) with a concrete retry. (Test #4 asserts
  the substring.)
- [ ] **AC — No regressions / gate green.** `scripts/verify.sh` passes; no `net`/`core`
  change; no schema/DB change; `blob_unavailable` is no longer `dead_code`; the fetch
  success path and `file share`/`file list` are unchanged.

**Test-plan coverage:** provider offline → AC1/AC4 (test #4); non-member → AC2 (test #3);
hash mismatch → invalid-hash AC (test #5).

---

## 12. Assumptions

1. The executing agent runs in a full dev checkout and may modify
   `crates/iroh-rooms-cli/{src/error.rs, src/file.rs, tests/*}`, `README.md`, and
   `docs/getting-started.md`; `scripts/verify.sh` is the gate.
2. `FetchOutcome`'s four non-`Fetched` variants are stable (IR-0204); the classifier
   consumes them read-only.
3. The `DeniedAtConnect`-vs-availability split (§5.2) is the right honesty rule for AC2;
   the alternative tie-breaks are OQ-2.
4. A new CLI-native `hash_mismatch` integrity code is preferable to overloading
   `invalid_content` (OQ-1); it is the only taxonomy addition.
5. No new availability guarantee is expected — "unavailable" is a truthful terminal
   state, not a deferred retry (PRD §14).
6. The IR-0204 fetch tier's loopback / `#[ignore]` test harness is available to trigger
   the online-only failure states deterministically.

## 13. Open questions

- **OQ-1 (integrity code).** Add `ErrorCode::HashMismatch` (`hash_mismatch`, exit 4;
  recommended) vs reuse `ErrorCode::Reject(RejectReason::InvalidContent)` (`invalid_content`,
  exit 4)? Recommend the dedicated code — it is script-distinct and does not overload an
  event-rejection code.
- **OQ-2 (unauthorized tie-break).** `Unauthorized` iff **all** attempts were
  `DeniedAtConnect` (recommended); or iff a *majority* were; or report a compound state
  when denials and unreachability mix? Recommend all-or-nothing — the simplest honest
  rule (any availability gap ⇒ "unavailable").
- **OQ-3 (`DeniedPerHash` classification).** Fold `DeniedPerHash` into `Unavailable`
  (recommended — the bytes are not obtainable now, with a retry hint) or give it a
  distinct message/code (e.g. `blob_not_referenced`)? Recommend fold-in for MVP; the
  per-provider stderr diagnostic already names it precisely.
- **OQ-4 (unsupported format code).** `invalid_argument` (exit 2, recommended — a build
  limitation the user's argument cannot fix) vs `blob_unavailable` (exit 6) vs a new
  `unsupported_format`? Recommend `invalid_argument` to keep the code set minimal.
- **OQ-5 (retire the reserved constant).** Delete `BLOB_UNAVAILABLE_MESSAGE` and inline
  the context-specific message (recommended, §5.5) vs keep it as a shared prefix?

## 14. Definition of done

1. `file fetch` reports its three failure classes distinctly and script-friendly:
   `blob_unavailable` (exit 6, PRD §14 language + retry) for no-online-provider,
   `peer_unauthorized`/`not_a_member` (exit 3) for the authorization wall, and
   `hash_mismatch` (exit 4) for a content-integrity failure — each a pinned
   `error[<code>]:` stderr line, nothing written to disk.
2. `ErrorCode::BlobUnavailable` is no longer `#[allow(dead_code)]`; the stale
   "not implemented" message is gone; `ErrorCode::HashMismatch` is added and pinned.
3. The aggregate classifier (`FetchTally`/`classify`) honestly splits unauthorized from
   unavailable per §5.2 and is unit-tested (including the mixed case).
4. `README.md` Error codes table and `docs/getting-started.md` "Unavailable file" case
   are reconciled to the shipped behaviour (coded line + exit code; the stale
   "file fetch not implemented" caveat removed); docs-conformance proves both codes
   emitted, not merely reserved.
5. `tests/file_cli.rs` covers provider-offline, non-member, and hash-mismatch (+ the
   all-refused split and ancillary paths); `scripts/verify.sh` is green.
6. No change to `net::blob`, the serve plane, the event schema, the DB, or any
   authorization verdict.
