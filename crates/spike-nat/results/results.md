# Gate A — rolled-up results

Rendered from the committed per-run JSON in this directory (format = [`report::results_md`](../src/report.rs)).
One row per (scenario, direction, path-mode) run. Matrix executed 2026-07-03/04 (scenario 1 of 2:
home-broadband ↔ hetzner-server; the likely-symmetric hotspot scenario is still owed — see NOTES.md §6).
The `settle30` rows (2026-07-04) are the issue-#43 reconciliation runs — `--settle 30 --xfer 0`, see §6.

| scenario | direction | mode | established | path type | ttfb (ms) | ttfb direct (ms) | ttfb relay (ms) | rtt median (ms) | throughput (Mbit/s) | setup (ms) |
|----------|-----------|------|-------------|-----------|-----------|------------------|-----------------|-----------------|---------------------|------------|
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 1005 | 1005 | — | 126.4 | 3.5 | 1267 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 638 | 638 | — | 126.6 | 0.7 | 928 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 712 | 712 | — | 113.5 | 3.8 | 1036 |
| home-broadband<->hetzner-server | BtoA | relay-only | yes | relay | 1074 | — | 1074 | 132.0 | 3.3 | 2035 |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1539 | 1539 | — | 109.1 | 1.1 | 1961 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 976 | 976 | — | 129.8 | 1.2 | 1685 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1373 | 1373 | — | 124.2 | 1.8 | 2214 |
| home-broadband<->hetzner-server | AtoB | relay-only | yes | relay | 1141 | — | 1141 | 144.1 | 1.2 | 2736 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1439 | 1439 | — | 121.8 | — | 4758 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1316 | 1316 | — | 128.0 | — | 1706 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1129 | 1129 | — | 126.3 | — | 1531 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 1731 | 1731 | — | 149.1 | — | 1998 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 753 | 753 | — | 131.6 | — | 1012 |
