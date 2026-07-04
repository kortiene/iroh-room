# Gate A — rolled-up results

Rendered from the committed per-run JSON in this directory (schema = [`report::results_md`](../src/report.rs), throughput/ttfb-direct/relay columns elided for width).
Findings + method caveats: [`../NOTES.md`](../NOTES.md) §6. **Both required environments measured** (2026-07-03/04):
S1 home-broadband NAT ↔ Hetzner cloud (non-symmetric); S2 cellular CGNAT ↔ {Hetzner cloud, home-broadband NAT} (the likely-symmetric environment).
The `settle30` rows are the issue-#43 `--settle 30` reconciliation runs (path-type only, `--xfer 0`).

## S1 — home-broadband ↔ hetzner-server (2026-07-03/04, 23 runs)

| scenario | direction | mode | established | path type | ttfb (ms) | rtt median (ms) | throughput (Mbit/s) | setup (ms) |
|---|---|---|---|---|---|---|---|---|
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 1005 | 126.4 | 3.5 | 1267 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 638 | 126.6 | 0.7 | 928 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 712 | 113.5 | 3.8 | 1036 |
| home-broadband<->hetzner-server | BtoA | relay-only | yes | relay | 1074 | 132.0 | 3.3 | 2035 |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | no | none | — | — | — | — |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1539 | 109.1 | 1.1 | 1961 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 976 | 129.8 | 1.2 | 1685 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1373 | 124.2 | 1.8 | 2214 |
| home-broadband<->hetzner-server | AtoB | relay-only | yes | relay | 1141 | 144.1 | 1.2 | 2736 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1439 | 121.8 | — | 4758 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1316 | 128.0 | — | 1706 |
| home-broadband<->hetzner-server | AtoB | natural | yes | mixed | 1129 | 126.3 | — | 1531 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 1731 | 149.1 | — | 1998 |
| home-broadband<->hetzner-server | BtoA | natural | yes | mixed | 753 | 131.6 | — | 1012 |

## S2 — cellular CGNAT hotspot ↔ {hetzner-server, home-broadband} (2026-07-04, 14 runs)

Mac on iPhone cellular Personal Hotspot (carrier CGNAT). `AtoB` = Mac dials peer; `BtoA` = peer dials Mac (inbound-to-CGNAT). Natural rows use `--xfer 0`; relay rows use a 256 KiB transfer.

| scenario | direction | mode | established | path type | ttfb (ms) | rtt median (ms) | throughput (Mbit/s) | setup (ms) |
|---|---|---|---|---|---|---|---|---|
| hotspot-cgnat<->hetzner-server | AtoB | natural | yes | mixed | 1127 | 162.7 | — | 1731 |
| hotspot-cgnat<->hetzner-server | AtoB | natural | yes | mixed | 1121 | 155.0 | — | 1858 |
| hotspot-cgnat<->hetzner-server | AtoB | natural | yes | mixed | 1482 | 166.4 | — | 2118 |
| hotspot-cgnat<->hetzner-server | AtoB | natural | yes | mixed | 1333 | 166.3 | — | 2036 |
| hotspot-cgnat<->hetzner-server | AtoB | relay-only | yes | relay | 1159 | 171.6 | 1.2 | 2828 |
| hotspot-cgnat<->hetzner-server | BtoA | natural | yes | mixed | 1683 | 180.9 | — | 1940 |
| hotspot-cgnat<->hetzner-server | BtoA | natural | yes | mixed | 1211 | 180.3 | — | 1482 |
| hotspot-cgnat<->hetzner-server | BtoA | natural | yes | mixed | 1360 | 179.9 | — | 1637 |
| hotspot-cgnat<->hetzner-server | BtoA | relay-only | yes | relay | 1207 | 297.8 | 0.2 | 2154 |
| hotspot-cgnat<->home-broadband | AtoB | natural | yes | mixed | 472 | 117.9 | — | 1333 |
| hotspot-cgnat<->home-broadband | AtoB | natural | yes | mixed | 434 | 106.5 | — | 1255 |
| hotspot-cgnat<->home-broadband | AtoB | natural | yes | mixed | 403 | 96.9 | — | 1127 |
| hotspot-cgnat<->home-broadband | AtoB | natural | yes | mixed | 650 | 131.3 | — | 1501 |
| hotspot-cgnat<->home-broadband | AtoB | relay-only | yes | relay | 648 | 113.2 | 0.1 | 2521 |
