# JSON-RPC (chatterd)

Transport: TCP, one JSON-RPC 2.0 object per line (newline-delimited JSON).

Default listen address: `127.0.0.1:9388` (set in `chatterd.toml` or `CHATTERD_CONFIG`).

This spec favors coarse-grained commands and a single server->client event stream per account. The event stream replaces polling-style verification and session status checks and survives UI reconnects.

## Architecture alignment (summary)

- `chatterd` is a long-lived daemon that owns Matrix session state and crypto. UIs are dumb clients.
- The daemon keeps syncing even when no UI is connected.
- Verification state is persisted by the daemon and re-emitted on reconnect.

## UI flow (high level)

- Create an account with `account.add`.
- Subscribe to events with `events.subscribe` (one subscription per account per TCP connection).
- Start login with `account.login_start`.
- Complete login with `account.login_complete` (SSO) or `account.login_password` (password).
- Restore sessions with `account.session_restore`.
- Initial data arrives via snapshot events if `snapshot:true`; use events for updates.
- Use verification commands; SAS details arrive via events.

## Methods

### rpc.ping
Request:
```json
{"jsonrpc":"2.0","id":1,"method":"rpc.ping"}
```
Response:
```json
{"jsonrpc":"2.0","result":"pong","id":1}
```

### rpc.version
Request:
```json
{"jsonrpc":"2.0","id":2,"method":"rpc.version"}
```
Response:
```json
{"jsonrpc":"2.0","result":"0.2.0","id":2}
```

### account.list
Request:
```json
{"jsonrpc":"2.0","id":3,"method":"account.list"}
```
Response:
```json
{"jsonrpc":"2.0","result":[{"account_id":"acct-1","homeserver":"https://example.com","login_method":null,"status":"needs_login","has_session":false}],"id":3}
```

### account.add
Params:
- `homeserver`: string (server name or URL; discovery performed)
- `login_method`: `sso` or `password` (optional; validated against discovery)

Request:
```json
{"jsonrpc":"2.0","id":4,"method":"account.add","params":{"homeserver":"failbox.xyz"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","homeserver":"https://matrix.failbox.xyz","login_method":null,"status":"needs_login","available_login_methods":["password","sso"],"login_flows":{"flows":[{"type":"m.login.password"},{"type":"m.login.sso"}]},"parse_warning":null},"id":4}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `login_flows` will contain raw JSON.

### events.subscribe
Start a single server->client event stream for an account on the current TCP connection. The daemon continues syncing even if the client disconnects. On reconnect, use `since` to catch up; if the cursor is too old, the server will emit `events.reset` and resend snapshots.

Params:
- `account_id`: string
- `since`: string (optional cursor for resuming)
- `snapshot`: bool (optional; default true). If true, server emits snapshot events for current account state, room list, and active verifications.

Request:
```json
{"jsonrpc":"2.0","id":5,"method":"events.subscribe","params":{"account_id":"acct-1","snapshot":true}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"subscription_id":"sub-1","account_id":"acct-1","since":"cursor-123"},"id":5}
```

### events.unsubscribe
Params:
- `subscription_id`: string

Request:
```json
{"jsonrpc":"2.0","id":6,"method":"events.unsubscribe","params":{"subscription_id":"sub-1"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"ok":true},"id":6}
```

### account.login_start
Params:
- `account_id`: string
- `login_method`: `sso` or `password` (optional; required if multiple methods and none selected)

Request:
```json
{"jsonrpc":"2.0","id":7,"method":"account.login_start","params":{"account_id":"acct-1","login_method":"sso"}}
```
Response (SSO):
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","login_method":"sso","sso_url":"https://example.com/_matrix/client/v3/login/sso/redirect","redirect_url":"http://127.0.0.1:9388/callback"},"id":7}
```
Response (password):
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","login_method":"password"},"id":7}
```

### account.login_complete
Params:
- `account_id`: string
- `token`: string (opaque; UI-provided)
- `device_name`: string (optional)

Request:
```json
{"jsonrpc":"2.0","id":8,"method":"account.login_complete","params":{"account_id":"acct-1","token":"opaque-token","device_name":"Chatter Desktop"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":8}
```

### account.login_password
Params:
- `account_id`: string
- `username`: string (Matrix user ID)
- `password`: string
- `device_name`: string (optional)

Request:
```json
{"jsonrpc":"2.0","id":9,"method":"account.login_password","params":{"account_id":"acct-1","username":"@alice:example.com","password":"secret","device_name":"Chatter Desktop"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":9}
```

### account.session_restore
Params:
- `account_id`: string

Request:
```json
{"jsonrpc":"2.0","id":10,"method":"account.session_restore","params":{"account_id":"acct-1"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":10}
```

### matrix.rooms_sync
Fetch initial room list; subsequent updates arrive via events.

Params:
- `account_id`: string

Request:
```json
{"jsonrpc":"2.0","id":11,"method":"matrix.rooms_sync","params":{"account_id":"acct-1"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","rooms":[{"room_id":"!room:example.com","name":"Chatter","is_encrypted":true,"is_direct":false}],"sync_token":"r0"},"id":11}
```

### matrix.room_messages
Fetch a page of messages; new messages arrive via events.

Params:
- `account_id`: string
- `room_id`: string
- `limit`: number (optional; default 20)
- `from`: string (optional pagination token)

Request:
```json
{"jsonrpc":"2.0","id":12,"method":"matrix.room_messages","params":{"account_id":"acct-1","room_id":"!room:example.com","limit":10}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"start":"t0","end":"t1","chunk":[{"event":"..."}]},"id":12}
```

### matrix.verification.request
Params:
- `account_id`: string
- `user_id`: string (`me`/`self` allowed to mean current account)
- `device_id`: string (required for self verification; optional otherwise)

Request:
```json
{"jsonrpc":"2.0","id":13,"method":"matrix.verification.request","params":{"account_id":"acct-1","user_id":"@alice:example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","user_id":"@alice:example.com","state":"requested"},"id":13}
```

### matrix.verification.accept
Params:
- `account_id`: string
- `user_id`: string (`me`/`self` allowed)
- `flow_id`: string

Request:
```json
{"jsonrpc":"2.0","id":14,"method":"matrix.verification.accept","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","status":"accepted"},"id":14}
```

### matrix.verification.confirm
Params:
- `account_id`: string
- `user_id`: string (`me`/`self` allowed)
- `flow_id`: string
- `match`: bool

Request:
```json
{"jsonrpc":"2.0","id":15,"method":"matrix.verification.confirm","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd","match":true}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","status":"confirmed"},"id":15}
```

### matrix.verification.cancel
Params:
- `account_id`: string
- `user_id`: string (`me`/`self` allowed)
- `flow_id`: string

Request:
```json
{"jsonrpc":"2.0","id":16,"method":"matrix.verification.cancel","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","status":"cancelled"},"id":16}
```

### matrix.login_flows
Params:
- `homeserver`: string (server name or URL; discovery performed)

Request:
```json
{"jsonrpc":"2.0","id":17,"method":"matrix.login_flows","params":{"homeserver":"https://example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"homeserver":"https://example.com","login":{"flows":[{"type":"m.login.password"}]}},"id":17}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `login` will contain raw JSON.

### matrix.capabilities
Params:
- `homeserver`: string (server name or URL; discovery performed)

Request:
```json
{"jsonrpc":"2.0","id":18,"method":"matrix.capabilities","params":{"homeserver":"https://example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"homeserver":"https://example.com","capabilities":{"m.change_password":{"enabled":true}}},"id":18}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `capabilities` will contain raw JSON.

### matrix.devices.list
List devices for a user. If `user_id` is omitted, lists devices for the current account user.

Params:
- `account_id`: string
- `user_id`: string (optional; `me`/`self` allowed)

Request:
```json
{"jsonrpc":"2.0","id":19,"method":"matrix.devices.list","params":{"account_id":"acct-1","user_id":"@alice:example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":[{"user_id":"@alice:example.com","device_id":"DEVICE","display_name":"Chatter Desktop","is_verified":false}],"id":19}
```

### matrix.verification.list
List active verification flows for an account.

Params:
- `account_id`: string
- `user_id`: string (optional; filter by user; `me`/`self` allowed)

Request:
```json
{"jsonrpc":"2.0","id":20,"method":"matrix.verification.list","params":{"account_id":"acct-1","user_id":"@alice:example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":[{"flow_id":"abcd","short_id":"abcd","user_id":"@alice:example.com","device_id":"DEVICE","stage":"sas"}],"id":20}
```

## Event stream

Event stream is delivered as JSON-RPC notifications on the same TCP connection used by `events.subscribe`:

```json
{"jsonrpc":"2.0","method":"events.push","params":{"subscription_id":"sub-1","account_id":"acct-1","seq":42,"cursor":"cursor-124","type":"matrix.verification.sas","data":{...}}}
```

If the `since` cursor is too old or invalid, the server emits `events.reset` and replays snapshots if `snapshot:true`.

### Event types

#### events.reset
```json
{"reason":"cursor_too_old|invalid_cursor","new_since":"cursor-200"}
```

#### account.state
Emitted on subscribe (snapshot) and when status changes.
```json
{"account_id":"acct-1","status":"needs_login|ready|error","has_session":false,"error":"...optional..."}
```

#### matrix.sync.state
Daemon-side sync status for the account.
```json
{"state":"syncing|idle|error|offline","error":"...optional..."}
```

#### matrix.rooms.snapshot
Emitted when `snapshot:true`. Full room list as of subscription time.
```json
{"rooms":[{"room_id":"...","name":"...","is_encrypted":true,"is_direct":false}]}
```

#### matrix.rooms.updated
Room list deltas.
```json
{"rooms":[{"room_id":"...","name":"...","is_encrypted":true,"is_direct":false}]}
```

#### matrix.room.message
```json
{"room_id":"!room:example.com","event":{"event":"..."}}
```

#### matrix.login.ready
```json
{"status":"ready","device_id":"DEVICE","user_id":"@alice:example.com"}
```

#### matrix.login.failed
```json
{"error":"..."}
```

#### matrix.verification.state
Emitted for each active verification on subscribe (snapshot) and on state changes.
```json
{"flow_id":"abcd","user_id":"@alice:example.com","device_id":"DEVICE","stage":"requested|accepted|sas|done|cancelled"}
```

#### matrix.verification.sas
Emitted when SAS is ready for presentation (including after reconnect if still active).
```json
{"flow_id":"abcd","user_id":"@alice:example.com","supports_emoji":true,"can_be_presented":true,"emoji":[{"symbol":"🦊","description":"fox"}],"decimals":[123,456,789]}
```
Either `emoji` or `decimals` may be present depending on the negotiated SAS method.

#### matrix.verification.done
```json
{"flow_id":"abcd","user_id":"@alice:example.com","reason":"verified"}
```

#### matrix.verification.cancelled
```json
{"flow_id":"abcd","user_id":"@alice:example.com","reason":"user_cancelled|mismatch|timeout|error|cancelled"}
```

## Migration map (old -> new)

- `account.add` -> `account.add`
- `login.start` -> `account.login_start`
- `login.complete` -> `account.login_complete`
- `matrix.login.password` -> `account.login_password`
- `matrix.login.sso_complete` -> `account.login_complete`
- `matrix.session.restore` -> `account.session_restore`
- `matrix.session.status` -> `events.subscribe` + `account.state` event
- `matrix.rooms.list` -> `matrix.rooms_sync` or `events.subscribe` snapshot + `matrix.rooms.snapshot`
- `matrix.room.messages` -> `matrix.room_messages` + `matrix.room.message` events
- `matrix.verification.request` -> `matrix.verification.request`
- `matrix.verification.accept` -> `matrix.verification.accept`
- `matrix.verification.start_sas` -> removed; use `matrix.verification.sas` event
- `matrix.verification.sas` -> removed; use `matrix.verification.sas` event
- `matrix.verification.confirm` -> `matrix.verification.confirm`
- `matrix.verification.cancel` -> `matrix.verification.cancel`
- `matrix.login_flows` -> unchanged (optional)
- `matrix.capabilities` -> unchanged (optional)

Unchanged: `rpc.ping`, `rpc.version`, `account.list`.

## State and persistence

Persisted by daemon (must survive UI restarts):
- Account registry, homeserver URL, login method.
- Matrix session and device data.
- E2EE crypto state and key material.
- Active verification flows (including SAS state if in progress).
- Event cursor for each account (optional but recommended for resume).

Ephemeral (can be reconstructed or re-fetched):
- TCP subscriptions and `subscription_id` values.
- Snapshot emission state for a given connection.
- In-flight command results.
