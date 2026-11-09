# 0005: decisions of the person on the briefs of 2026-11-08

Status: decided by the person (not provisional), 2026-11-09. The briefs are in
[digest 2026-11-08](../digests/2026-11-08.md), section "The backlog and decision briefs".

## B-1. "Ruum" and "ruum" in Estonian

The product name is written **Ruum**, with a capital letter. The common Estonian word for a space is
written **ruum**, all lower case. Both may appear in Estonian text; the capital letter tells them
apart. So that the difference stays visible, a sentence does not start with the common word "ruum"
(rephrase instead, for example "Sinu ruum ..."). Avoid putting the two words side by side; when no
other wording works, "Ruum ruum" is fine. Plan names stay English; "tööhoog" stays until a
better word is found. The Estonian style guide (`docs/ux/style-guides/et.md`) and the translation check
follow this.

## B-2. The masking rule for tenant-defined types

A personal column whose type the tenant defined (enum, domain, composite, range, or any type that is
not a built-in base type) is masked **like `secret`**: agents see the hidden-value marker, never a
pseudonym. No tenant-defined conversion ever runs in a definer context. Agents cannot join or group on
such a column. This closes the hole of `review-dbhost-2.md` item 2 together with the hardening of the
definer functions (backlog S-1, S-4), before card Q-2.

## B-3. Text that one agent writes and other agents read

Every field that an agent writes and other agents or people read (for example the reason of a column
class suggestion, approvals, issue text) is marked **untrusted** in the contract (an additive
`content_trust: untrusted`, as for file content), and the MCP tool texts tell agents to treat it as
data, never as instructions. This is one rule for all such fields, current and future.

## B-4. The sqlx SQLite stand-in crate

The person approved the empty stand-in crate `third_party/sqlx-sqlite-stub` (wired through
`[patch.crates-io]`), which lets `rusqlite` 0.41.1 fix RUSTSEC-2026-0459 while `sqlx` 0.9.0 still pins
an older `libsqlite3-sys` for a driver Ruum never builds. Remove it when a `sqlx` release allows
`libsqlite3-sys` 0.39 or later. Decided 2026-11-09.
