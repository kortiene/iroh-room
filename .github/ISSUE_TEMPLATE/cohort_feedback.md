---
name: Cohort feedback
about: Share first-cohort feedback without leaking private room data
title: "[cohort] "
labels: ["needs-triage"]
assignees: ""
---

## Security And Privacy Warning

Do not attach or paste:

- `identity.secret`
- full invite tickets (`roomtkt1...`)
- invite capability secrets
- `rooms.db`
- blob files or private artifacts
- `audit.ndjson` without redaction
- full data-directory backups
- full terminal transcripts that include tickets or secrets

Redact sensitive values before posting. If this report involves a suspected
secret leak, unauthorized room access, unauthorized pipe access, data loss, or
remote code execution risk, say so at the top and avoid sharing reproduction
data publicly.

## Cohort Workflow

Which workflow did you try?

- [ ] Two-human room
- [ ] Verified file share/fetch
- [ ] Live Pipe localhost preview
- [ ] Agent status/artifact workflow
- [ ] Other:

## Outcome

- [ ] Completed without help
- [ ] Completed with maintainer help
- [ ] Partially completed
- [ ] Failed

## What Were You Trying To Do?

Describe the real task, not only the command.

Example: "Share a local Next.js preview with a reviewer" or "Fetch a build
artifact from another machine."

## Commands Run

Paste commands with secrets removed.

```bash
# example
iroh-rooms room join roomtkt1<redacted>
```

## Error Or Warning Output

Paste only relevant redacted lines. Prefer `error[code]`, `warning[code]`,
`next:`, and `diag:` lines.

```text
error[...]: ...
next: ...
```

## Environment

- OS:
- Architecture:
- Iroh Rooms version or commit SHA:
- Install method: release artifact / source build / other
- Rust version, if built from source:
- `IROH_ROOMS_HOME` set? `<yes/no, path redacted if sensitive>`

## Network Context

- Number of peers:
- Same machine / LAN / home NAT / cellular hotspot / relay-only / unknown:
- Was the peer process online? `<yes/no/unknown>`
- Did you pass `--peer` manually? `<yes/no>`
- If Live Pipe: exposed target was loopback only? `<yes/no>`

## Friction

What was confusing, slow, or surprising?

1.
2.
3.

## Would You Use This Again?

- [ ] Yes, for a real workflow next week
- [ ] Maybe, after fixes
- [ ] No

Why?

## Missing Feature Or Rough Edge

What is the smallest change that would make this useful for you?

## Additional Context

Add screenshots or logs only after checking they do not include secrets, full
tickets, `rooms.db`, unredacted `audit.ndjson`, full data-directory backups, or
private blob contents.
