# `spike-N40` matrix results

Rendered from `n40-probe matrix` (loopback `NetMode::Loopback`, no relay/discovery). Regenerate with the command documented in `crates/spike-N40/results/README.md`.

| N | rate events/s | mode | survives? | rss total MiB | rss/node est MiB | dial loops/node | writer+reader tasks/node est | connected entries | accepted min/max | frames_sent min/max | queue saturations | reconnects/sec | cascade? |
|---:|---:|---|---|---:|---:|---:|---:|---:|---|---|---:|---:|---|
| 5 | idle | idle | yes | 59 | 10 | 4 | 8 | 20/15 | 0/0 | 0/0 | 0 | 0.00 | no |
| 5 | 1 | load | yes | 69 | 2 | 4 | 8 | 20/15 | 30/30 | 90/123 | 0 | 0.00 | no |
| 5 | 5 | load | yes | 78 | 2 | 4 | 8 | 20/15 | 150/150 | 450/600 | 0 | 0.00 | no |
| 40 | idle | idle | yes | 178 | 3 | 39 | 12 | 240/120 | 0/1 | 0/10 | 0 | 0.13 | no |
| 40 | 1 | load | yes | 258 | 2 | 39 | 12 | 240/120 | 30/30 | 150/183 | 0 | 0.00 | no |
| 40 | 5 | load | yes | 304 | 1 | 39 | 12 | 240/120 | 150/150 | 758/952 | 0 | 0.00 | no |
