# chattergtk (test GUI)

`chattergtk` is a minimal GTK4 test client for `chatterd`.

Current scope:
- Single account only.
- Assumes account login/setup is already done.
- Reads room list.
- Reads text messages for selected room.
- Sends text messages.
- Message timeline updates only from `events.subscribe` (`room.message`), not optimistic local echo.

## Requirements

- `chatterd` running (`127.0.0.1:9388` by default).
- Python 3.
- GTK4 Python bindings (`PyGObject`).

On Debian/Ubuntu-like systems, packages are typically:
- `python3-gi`
- `gir1.2-gtk-4.0`

## Run

```bash
python3 tools/chattergtk.py
```

## Notes

- Before connecting, only connection controls are shown.
- Use `Connect...` to open the connection dialog (daemon address + optional account id).
- Connect picks the first `ready` account from `account.list` unless `Account` is filled.
- `Reload Rooms` calls `room.list`.
- Room list shows friendly names only.
- Selecting a room highlights it; press `Enter` in the room list to open it and call `room.messages`.
- Messages are shown in a scrollable list.
- `Send` calls `room.send`; UI waits for `room.message` event to show the message.
- If you receive `events.reset`, the app updates cursor and reloads the selected room.
