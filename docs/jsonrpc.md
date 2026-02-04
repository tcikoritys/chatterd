# JSON-RPC (chatterd)

Transport: TCP, one JSON-RPC 2.0 request per line (newline-delimited JSON).

Default listen address: `127.0.0.1:9388` (set in `chatterd.toml` or `CHATTERD_CONFIG`).

## UI flow

- Discover homeserver login options with `matrix.login_flows` and `matrix.capabilities`.
- Create an account via `account.add`.
- If login is SSO: UI opens `sso_url`, completes the browser flow, then calls `matrix.login.sso_complete` with the token it obtains.
- If login is password: UI collects credentials and calls `matrix.login.password`.
- On app restart, call `matrix.session.restore` for accounts that have `has_session: true`.
- Use `matrix.rooms.list` and `matrix.room.messages` to render the room list and timeline.
- Use `matrix.verification.*` to handle SAS emoji/decimal verification for E2EE.

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
{"jsonrpc":"2.0","result":"0.1.0","id":2}
```

### account.list
Request:
```json
{"jsonrpc":"2.0","id":3,"method":"account.list"}
```
Response:
```json
{"jsonrpc":"2.0","result":[],"id":3}
```

### account.add
Params:
- `homeserver`: string
- `login_method`: `sso` or `password` (optional; defaults to `sso`)

Request:
```json
{"jsonrpc":"2.0","id":4,"method":"account.add","params":{"homeserver":"https://example.com","login_method":"sso"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"id":"acct-1","homeserver":"https://example.com","login_method":"sso","status":"needs_login"},"id":4}
```

### login.start
Params:
- `account_id`: string

Request:
```json
{"jsonrpc":"2.0","id":5,"method":"login.start","params":{"account_id":"acct-1"}}
```
Response (SSO):
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","login_method":"sso","sso_url":"https://example.com/_matrix/client/v3/login/sso/redirect","redirect_url":"http://127.0.0.1:9388/callback"},"id":5}
```

### login.complete
Params:
- `account_id`: string
- `token`: string (opaque; UI-provided)

Request:
```json
{"jsonrpc":"2.0","id":6,"method":"login.complete","params":{"account_id":"acct-1","token":"opaque-token"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":6}
```

### matrix.session.status
Request:
```json
{"jsonrpc":"2.0","id":7,"method":"matrix.session.status"}
```
Response:
```json
{"jsonrpc":"2.0","result":[{"account_id":"acct-1","homeserver":"https://example.com","status":"ready","has_session":true}],"id":7}
```

### matrix.login.password
Params:
- `account_id`: string
- `username`: string (Matrix user ID)
- `password`: string
- `device_name`: string (optional)

Request:
```json
{"jsonrpc":"2.0","id":8,"method":"matrix.login.password","params":{"account_id":"acct-1","username":"@alice:example.com","password":"secret","device_name":"Chatter Desktop"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":8}
```

### matrix.login.sso_complete
Params:
- `account_id`: string
- `token`: string (login token from the UI flow)
- `device_name`: string (optional)

Request:
```json
{"jsonrpc":"2.0","id":9,"method":"matrix.login.sso_complete","params":{"account_id":"acct-1","token":"sso-token","device_name":"Chatter Desktop"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":9}
```

### matrix.session.restore
Params:
- `account_id`: string

Request:
```json
{"jsonrpc":"2.0","id":10,"method":"matrix.session.restore","params":{"account_id":"acct-1"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"account_id":"acct-1","status":"ready"},"id":10}
```

### matrix.rooms.list
Params:
- `account_id`: string

Request:
```json
{"jsonrpc":"2.0","id":11,"method":"matrix.rooms.list","params":{"account_id":"acct-1"}}
```
Response:
```json
{"jsonrpc":"2.0","result":[{"room_id":"!room:example.com","name":"Chatter","is_encrypted":true,"is_direct":false}],"id":11}
```

### matrix.room.messages
Params:
- `account_id`: string
- `room_id`: string
- `limit`: number (optional; default 20)

Request:
```json
{"jsonrpc":"2.0","id":12,"method":"matrix.room.messages","params":{"account_id":"acct-1","room_id":"!room:example.com","limit":10}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"start":"t0","end":"t1","chunk":[{"event":"..."}]},"id":12}
```

### matrix.verification.request
Params:
- `account_id`: string
- `user_id`: string
- `device_id`: string (optional; if omitted, request identity verification)

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
- `user_id`: string
- `flow_id`: string

Request:
```json
{"jsonrpc":"2.0","id":14,"method":"matrix.verification.accept","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","status":"accepted"},"id":14}
```

### matrix.verification.start_sas
Params:
- `account_id`: string
- `user_id`: string
- `flow_id`: string

Request:
```json
{"jsonrpc":"2.0","id":15,"method":"matrix.verification.start_sas","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"supports_emoji":true,"can_be_presented":true,"is_done":false,"is_cancelled":false,"emoji":[{"symbol":"🦊","description":"fox"}]},"id":15}
```

### matrix.verification.sas
Params:
- `account_id`: string
- `user_id`: string
- `flow_id`: string

Request:
```json
{"jsonrpc":"2.0","id":16,"method":"matrix.verification.sas","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"supports_emoji":true,"can_be_presented":true,"is_done":false,"is_cancelled":false,"decimals":[123,456,789]},"id":16}
```

### matrix.verification.confirm
Params:
- `account_id`: string
- `user_id`: string
- `flow_id`: string
- `match`: bool

Request:
```json
{"jsonrpc":"2.0","id":17,"method":"matrix.verification.confirm","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd","match":true}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","status":"confirmed"},"id":17}
```

### matrix.verification.cancel
Params:
- `account_id`: string
- `user_id`: string
- `flow_id`: string

Request:
```json
{"jsonrpc":"2.0","id":18,"method":"matrix.verification.cancel","params":{"account_id":"acct-1","user_id":"@alice:example.com","flow_id":"abcd"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"flow_id":"abcd","status":"cancelled"},"id":18}
```

### matrix.login_flows
Params:
- `homeserver`: string (hostname or URL)

Request:
```json
{"jsonrpc":"2.0","id":19,"method":"matrix.login_flows","params":{"homeserver":"https://example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"homeserver":"https://example.com","login":{"flows":[{"type":"m.login.password"}]}},"id":19}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `login` will contain raw JSON.

### matrix.capabilities
Params:
- `homeserver`: string (hostname or URL)

Request:
```json
{"jsonrpc":"2.0","id":20,"method":"matrix.capabilities","params":{"homeserver":"https://example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"homeserver":"https://example.com","capabilities":{"m.change_password":{"enabled":true}}},"id":20}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `capabilities` will contain raw JSON.
