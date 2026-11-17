# Builder fixtures (card R-2b)

Each folder is the source of one `node22` archive; `tests/build.rs` packs a folder into a tar
(and adds generated files where a case needs them, such as the hostile build's `targets.json`).

| Folder | What the build does |
|---|---|
| `hello` | Builds; `server.js` answers `hello from ruum` on `$PORT`. |
| `failing` | `postinstall` exits 3: `build_failed`. |
| `slow` | `postinstall` sleeps ten minutes: `build_timed_out`. |
| `envdump` | `postinstall` writes `env-dump:begin`, every variable as `NAME=<hex(value)>`, the readable credential files as `FILE:<path>=<hex(content)>`, `env-dump:end` (backlog S-37). |
| `hostile` | Installs `ms` from the npm registry through the egress proxy, then its `postinstall` probes the targets of `targets.json` (TCP, the metadata HTTP GET, DNS, CONNECT through the proxy) and prints one `probe <json>` line per target. |
| `huge` | `postinstall` writes `size.txt` MiB into the work folder (over the disk limit: `ENOSPC`; under it but over the output cap: the byte cap). |
| `forkbomb` | `postinstall` starts 2000 processes; the pids limit stops it, and it exits 1. |
| `names` | `postinstall` makes files with hostile names, links out of the tree, a link inside it, a FIFO and a hard link; the builder keeps the files and the inside link and drops the rest. |

The lock files list no package except `hostile`'s `ms` 2.1.3 (from spike S-2), so the other
builds need no network.
