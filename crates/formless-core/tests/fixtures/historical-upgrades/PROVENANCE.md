# Historical upgrade fixtures

`prefix-through-46.sql` contains ordered migration SQL copied from
`crates/formless-core/src/storage.rs` at
`60554b7fc77ba0537d945e26a49bbd9f4498ea2f`. Constants are expanded in place;
`add_column` calls become their historical `ALTER TABLE ... ADD COLUMN`
statements. The v39 indexes, summary backfill and triggers retain their
original execution order. No current Store is opened to construct a fixture.

The checkpoints have these historical anchors:

| Checkpoint | Source commit |
| --- | --- |
| 1, 3 | `419c822ddb4635e179e51c1f65e9cbfa5e999d5b` (original migration steps) |
| 17 | `c494852602d403b7c5e8090b884c32bd5b265578` |
| 23 | `8106e10f814cf1ffdc8dd30fb575adb4a6fbe8af` |
| 38 | `08d4502c26b44d102ef029d1b7eb44496e3f1a0e` |
| 46 | `60554b7fc77ba0537d945e26a49bbd9f4498ea2f` (step before v47) |

Checkpoints 1, 3 and 46 are actual migration prefixes, even though no
separate `SCHEMA_VERSION` declaration advertised each one. Tests replay
only steps 1 through the checkpoint and stamp each successfully applied
step, seed old columns directly, then invoke the production upgrade.
The upgrade destination is `storage::SCHEMA_VERSION`, currently 57, rather
than the issue's historical target of 47.

`config-v1.toml` uses fields accepted by `Config` at the original commit
`419c822ddb4635e179e51c1f65e9cbfa5e999d5b`; the endpoint is inert and the model
name is a fixture. The key is generated as an explicitly fake private file
at test time; it is never saved to an OS keychain. `artifact.txt` is fixture
content, registered using the artifact table already present at v23.

Tests embed these files with `include_str!` / `include_bytes!`; shallow
checkouts and offline execution need neither Git history nor a Git binary.
Do not regenerate the fixtures from the latest database or re-stamp a
current schema as an old version. Extend assertions when adding a new
checkpoint and preserve these historical inputs.
