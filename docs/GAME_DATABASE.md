# Game database

Cellar can connect to a hosted MySQL or MariaDB database and give an operator a
safe, responsive view of the live schema. The gamemode owns the database
contract. It supplies the migrations, table names, columns, indexes, and data
meaning. Cellar does not create game tables or apply gamemode migrations.

Set the connection string outside the config file:

```sh
export CELLAR_DATABASE_URL='mysql://cellar_readonly:password@db.example.net/game'
```

Then enable the connection in `cellar.toml`:

```toml
[database]
enabled = true
schema_owner = "gamemode"
migrate_on_start = false
```

The web UI Database panel provides:

- live connection, server version, table count, and storage size;
- table discovery from `information_schema`;
- column details and paged table browsing;
- one read-only query at a time, capped at 500 rows;
- explicit ownership text so an operator knows Cellar is not the source of the schema.

Use a database account with `SELECT` and metadata permissions only. The query
shape check catches accidental writes and multiple statements, but it is not a
security boundary. The database grant is the security boundary.

Cellar's old operational migration switch remains accepted for v0.1 config
files. It is ignored when `schema_owner = "gamemode"`, with a warning at
startup. This prevents an upgrade from silently mutating a live game database.
