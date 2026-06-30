# Iroh Rooms ADW Pack

This directory contains the Switchyard / ADW project pack for Iroh Rooms.

The pack is intentionally thin:

- `.adw/config.json` defines GitHub providers, branch naming, gates, and model tiers.
- `.adw/prompts/` contains the neutral Switchyard phase prompts used by the ADW kernel.
- The project-specific source of truth remains `README.md`, `PRD.v0.3.md`, and `PHASE-0-SPIKE.md`.

Run ADW only from a clean working tree, and do not use automatic merge flags on
`priority/p0`, `risk/high`, `area/protocol`, `area/transport`, `area/pipe`, or
`type/security` issues.

External Switchyard runs must pass this repository as the project root:

```bash
cd /path/to/switchyard/adw_sdlc
npm run issue -- <issue-number> \
  --repo kortiene/iroh-room \
  --project-root /path/to/iroh-room \
  --runner claude \
  --dry-run
```
