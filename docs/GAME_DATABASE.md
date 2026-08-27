# Game database

Cellar can connect to a hosted MySQL or MariaDB database and give an operator a
safe, responsive view of the live schema. The gamemode owns the database
contract. It supplies the migrations, table names, columns, indexes, and data
meaning. Cellar does not create game tables or apply gamemode migrations.

Cellar supports two database sources. Set the connection string outside the
config file through `CELLAR_DATABASE_URL`:

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

For an imported hosted source, store the URL in a protected one-line file
outside the repository and point Cellar at it instead:

```toml
[database]
enabled = true
url_file = "C:/ProgramData/Cellar/external-database.url"
schema_owner = "gamemode"
migrate_on_start = false
```

The file contains one `mysql://` or `mariadb://` URL. `CELLAR_DATABASE_URL`
takes precedence when both are present. Cellar reads the file at startup,
never writes the password to TOML, and never copies the source database into
the AppleJackRP repository. Set `[mariadb].managed = false` when the database
is hosted elsewhere. `cellar doctor` validates reachability before starting
the server.

The Database panel is protected by the same web authentication as the control
surface. For a phone or any non-loopback bind, use password auth:

```toml
[web]
enabled = true
bind = "127.0.0.1:8081" # or a private tailnet address
auth = "password"
```

Generate the hash with `cellar hash-password` and provide it through
`CELLAR_WEB_PASSWORD_HASH`. For local-only development, `auth = "none"` is
available on loopback.

The web UI Database panel provides:

- live connection, server version, table count, and storage size;
- table discovery from `information_schema`;
- column details and paged table browsing;
- one read-only query at a time, capped at 500 rows;
- explicit ownership text so an operator knows Cellar is not the source of the schema.

## Prometheus and Grafana

Cellar exposes the current operational metrics in Prometheus text format at
`/metrics`. It includes server state, players, uptime, restarts, parser health,
process and host CPU/memory, network rates, bridge counters, and database
connectivity. It also labels the configured database source as `managed`,
`external`, or `disabled`. Protect the endpoint with the read-only bearer token:

```toml
[web]
enabled = true
bind = "127.0.0.1:8081"

# Prometheus sends: Authorization: Bearer <CELLAR_API_TOKEN>
```

Set `CELLAR_API_TOKEN` in the Cellar process environment, then configure
Prometheus to scrape `http://127.0.0.1:8081/metrics`. Grafana can use
Prometheus as its data source and graph the `cellar_*` series. The JSON
equivalents remain available under `/api/v1/status`, `/api/v1/resources`, and
`/api/v1/addresses` for other hosted dashboards.

Use a database account with `SELECT` and metadata permissions only. The query
shape check catches accidental writes and multiple statements, but it is not a
security boundary. The database grant is the security boundary.

Cellar's old operational migration switch remains accepted for v0.1 config
files. It is ignored when `schema_owner = "gamemode"`, with a warning at
startup. This prevents an upgrade from silently mutating a live game database.
