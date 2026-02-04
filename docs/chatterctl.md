# chatterctl

`chatterctl` is an interactive REPL for driving the `chatterd` JSON-RPC API.

Example session:

```text
account add https://example.com
events subscribe acct-1
account login-start acct-1
account login-complete acct-1 TOKEN
room list acct-1
room messages acct-1 !room:example.com
room send acct-1 !room:example.com hello
verification request acct-1 me
verification confirm acct-1 me
help
```
