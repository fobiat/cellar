# The bridge

AppleJackRP's `Documentation/Design/20_PERSISTENCE.md` §6.3 specifies an HTTP
document store the gamemode talks to instead of local JSON files. The gamemode's
client half is written, tested and shipped: `Code/Storage/HostedDocumentStore.cs`,
`HostedDocumentProtocol.cs`, `HostingConfigStore.cs`, and their rule tests.

Nothing implemented the server half. A search for `v1/doc` across every repo on
this machine found only the spec, the client, and the client's tests. So
`StorageDirector` logged, accurately:

> `hosted: the call sites are still synchronous, so every read answers
> Unavailable and every write stays owed in the journal`

Cellar is that missing half. "MySQL support" is not a feature bolted on; it is
the implementation of a contract this project already wrote down.

---

## The protocol

Read from the shipped C# client, not invented.

| Method | Route | Answers |
| --- | --- | --- |
| `GET` | `/v1/doc/{key}` | `200` with a JSON body, `404` absent, anything else unavailable |
| `PUT` | `/v1/doc/{key}` | `2xx` written, `409` stale revision, anything else failed |
| `HEAD` | `/v1/doc/{key}` | `200` present, `404` absent |

### `404` is the only status that may mean "absent"

This is the single most important rule in the whole system, and it is the reason
§4.1 of the spec exists.

Map a `500`, a timeout, or a connection refusal to "absent" and this happens: a
player joins, their character roster reads as empty, the gamemode treats that as
a new player, and the empty roster is written back over their real character.
The data is gone and nothing errored.

So every non-2xx that is not exactly `404` means **"I cannot tell you"**, and the
client's circuit breaker handles it. Cellar has a test per status code the client
distinguishes, ported from `RuleTests/HostedDocumentProtocolTests.cs`, so the two
halves are provably talking about the same protocol.

### Timeouts

The client uses **3s for reads and 5s for writes**, and opens its circuit breaker
after three consecutive failures. The bridge has to answer well inside that.

### Authentication

`Authorization: Bearer <token>`, from `Sandbox.Services.Auth.GetToken(audience)`,
on every request. What Cellar does with it depends on `bridge.auth`; see
[Configuration](CONFIGURATION.md#the-three-auth-modes-honestly). The short
version is that the default mode does not verify the token and says so.

**`public_url` must never redirect.** `SboxHttpHandler.ApplyRedirect` strips the
`Authorization` header on every hop, so a redirecting URL silently turns every
authenticated request into an unauthenticated one.

### The dropped path segment

The original spec had a `{scope}` segment in the route. **The shipped client does
not send it.** Cellar matches the client, not the spec. Scope is a column, set
from config, not a path segment.

---

## Document keys

Ported exactly from `Code/Storage/DocumentKeys.cs`:

- characters `[a-z0-9._-]`, with `/` as the separator
- at most 128 characters
- no leading or trailing separator
- no `.` or `..` segment
- no reserved device name (`con`, `nul`, `com1` …)

Cellar **refuses** an illegal key rather than sanitising it, matching the C#
posture. Sanitising would mean two different keys quietly becoming one.

These rules also make a key safe as a `VARCHAR(128)` primary key and safe in a
URL path, which is not a coincidence.

The five documents that land here:

| Key | Holds |
| --- | --- |
| `characters/<steamid>.json` | One player's characters |
| `features.json` | Feature toggles |
| `laws.json` | The law catalogue |
| `permissions.json` | Permission grants |
| `doors/<map>.json` | Door ownership for one map |

---

## Schema

```sql
aj_document           scope, doc_key(128), body JSON, revision, updated_at, updated_by
                      PRIMARY KEY (scope, doc_key)
aj_document_revision  append-only history of every write
```

`aj_document_revision` answers "who overwrote my character", which is the
question you will eventually need answered at speed. `cellar doc history <key>`
reads it.

The operational tables are separate and prunable:

```sql
srv_session         id, started_at, ended_at, exit_code, exit_reason, image_tag, host
srv_player          steamid, last_name, first_seen, last_seen, total_seconds
srv_player_session  session_id, steamid, name, joined_at, left_at, leave_reason
srv_event           session_id, at, kind, logger, steamid, payload JSON
srv_command         session_id, at, actor, command, reply, ok
```

`cellar db prune` deletes from `srv_event` by `database.event_retention_days`. It
never touches `aj_document` or its revisions.

### Revisions: recorded, but never rejected

Cellar increments the revision on every write and records what a conflict *would*
have been, but **answers `204` and never `409`**.

That is deliberate. The shipped client's own doc comment says it "Never returns
`Rejected`, §3.4 waits on §11 Q1". Answering `409` to a client that cannot act on
it turns a recoverable write into a lost one. The conflict count is exposed so
you can see whether real concurrency exists before turning anything on.

Real optimistic concurrency becomes a config flag once the gamemode side lands.

### Scope

`scope` is populated from config and defaults to one value, which answers the
spec's open question Q1 ("one server's data off-box, or many servers shared?") as
**one server**. §11 says that question changes the cost of a hosted provider more
than any other, so it is answered explicitly rather than by accident.

The column exists so the other reading is a migration rather than a rewrite. None
of §3.4's revision-conflict machinery or §7.2's cross-server isolation is built.

---

## Wiring it to AppleJackRP

`HostingConfigStore` reads `hosting.json` from `FileSystem.Data` and **refuses
malformed input loudly** rather than falling back to local storage. That is
deliberate on the gamemode's part, so a typo cannot silently send a hosted
server's writes to disk.

Cellar writes that file for you, before launching the child, from its own config:

```json
{
  "version": 1,
  "provider": "hosted",
  "bridgeUrl": "http://127.0.0.1:8080",
  "authAudience": "applejack-bridge",
  "apiKey": "cellar-local-trusted"
}
```

It goes in `server.data_dir`. **If `data_dir` is unset, this never happens and
the gamemode keeps using local files with no error at all.** `cellar doctor`
checks for it.

For a loopback `trusted` bridge, Cellar writes the local-only `apiKey` shown
above. This avoids depending on the platform token service during local
dedicated-server runs. It is not a remote credential: trusted mode is refused
on a reachable bind. Shared-secret mode writes the configured secret instead.

One source of truth, and no hand-editing of a file that refuses to be
hand-edited.

### `-allowlocalhttp`

Cellar adds this to the launch line when, and only when, the bridge is enabled.

Without it, `Http.IsAllowed` (`engine/Sandbox.Engine/Utility/Web/Http.cs`) blocks
direct IP literals, any host resolving to a private or loopback address, and
loopback on ports other than 80, 443, 8080 and 8443. A bridge on a
cluster-internal address is a private address, so the gamemode would refuse to
reach it and you would see nothing but silence.

The flag short-circuits all of those checks, and it takes effect only for the
editor and the dedicated server.

---

## Checking it works

```sh
cellar doc ls                 # anything at all?
cellar doc ls characters/     # characters specifically
```

By hand, against a running bridge:

```sh
curl -i -H 'Authorization: Bearer test' http://127.0.0.1:8080/v1/doc/features.json
curl -i -X HEAD -H 'Authorization: Bearer test' http://127.0.0.1:8080/v1/doc/nope.json
```

The second must be exactly `404`. If it is `500`, something is wrong and a
character is at risk.

The real end-to-end test: create a character, delete the pod, rejoin, and confirm
the character is intact.
