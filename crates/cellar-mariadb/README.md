# cellar-mariadb

Provides managed database provisioning and supervision, plus native logical
backup and restore for managed or external databases. `backup.rs` owns the
shared connection policy and client selection. Public entry points are
`backup`, `restore`, `provision` and `Supervisor`.

Backup clients ignore option files, use `utf8mb4`, pass passwords through the
child environment, and refuse connection options they cannot preserve. Restore
requires trusted SQL; file checks establish completion rather than authority.
See [configuration](../../docs/CONFIGURATION.md#native-backup-connections).

Run `cargo test -p cellar-mariadb --lib backup::tests` for focused checks.
For real charset and Unicode/binary acceptance, set `CELLAR_TEST_DATABASE_URL`
to a disposable database, install `mariadb` and `mariadb-dump` on `PATH`, then
run the focused library checks and `cargo test -p cellar-mariadb --test roundtrip`.
The real tests skip when no database is supplied. Managed provisioning is
Windows-only; backup clients also run on Linux.
