# JSON-RPC (chatterd)

Transport: TCP, one JSON-RPC 2.0 request per line (newline-delimited JSON).

Default listen address: `127.0.0.1:9388` (set in `chatterd.toml` or `CHATTERD_CONFIG`).

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

### matrix.login_flows
Params:
- `homeserver`: string (hostname or URL)

Request:
```json
{"jsonrpc":"2.0","id":7,"method":"matrix.login_flows","params":{"homeserver":"https://example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"homeserver":"https://example.com","login":{"flows":[{"type":"m.login.password"}]}},"id":7}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `login` will contain raw JSON.

### matrix.capabilities
Params:
- `homeserver`: string (hostname or URL)

Request:
```json
{"jsonrpc":"2.0","id":8,"method":"matrix.capabilities","params":{"homeserver":"https://example.com"}}
```
Response:
```json
{"jsonrpc":"2.0","result":{"homeserver":"https://example.com","capabilities":{"m.change_password":{"enabled":true}}},"id":8}
```
If the homeserver returns a non-spec response, `parse_warning` is included and `capabilities` will contain raw JSON.
