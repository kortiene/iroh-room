---
name: Bug report
about: Report a bug without leaking room secrets or private data
title: "[bug] "
labels: ["bug", "needs-triage"]
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
remote code execution risk, mark it clearly at the top and avoid sharing
reproduction data publicly.

## Summary

What happened?

## Expected Behavior

What did you expect to happen?

## Actual Behavior

What happened instead?

## Commands Run

Paste commands with secrets removed.

```bash
# example
iroh-rooms room join roomtkt1<redacted>
```

## Error Or Warning Output

Paste only the relevant lines. Prefer `error[code]`, `warning[code]`, and
`next:` lines.

```text
error[...]: ...
next: ...
```

## Environment

- OS:
- Architecture:
- Iroh Rooms version or commit SHA:
- Rust version, if built from source:
- Install method:
- `IROH_ROOMS_HOME` set? `<yes/no, path redacted if sensitive>`

## Room Context

Redact if sensitive.

- Number of peers:
- Human/agent participants:
- Command area: identity / room / invite / join / message / file / pipe / agent
- Network mode: same machine / LAN / home NAT / cellular hotspot / relay-only / unknown

## Reproduction Steps

1.
2.
3.

## Data Safety

- Did this involve data loss? `<yes/no/unknown>`
- Did this involve a secret or ticket leak? `<yes/no/unknown>`
- Did this involve unauthorized access? `<yes/no/unknown>`
- Did you make a backup before upgrade or restore? `<yes/no/not applicable>`

## Additional Context

Add screenshots or logs only after checking they do not include secrets, full
tickets, `rooms.db`, unredacted `audit.ndjson`, full data-directory backups, or
private blob contents.
