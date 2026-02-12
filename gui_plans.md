# GUI Plans

Near-term improvements for the `chattergtk` test client.

## 1. Message history pagination

- Add a `Load older` action for the selected room.
- Use `room.messages` with `from=<end token from previous page>`.
- Append older messages at the top while preserving current scroll position.
- Keep ordering oldest -> newest in the viewport.

## 2. Read marker scaffold

- Track a local `last_read` marker per room in the test client.
- Mark the boundary in the message list with a simple divider row.
- Start local-only first; later align with server/account state behavior.

## 3. Better non-text rendering

- Bucket common event types for clearer browsing:
  - `m.image`, `m.file`, `m.audio`, `m.video`
  - reactions (`m.reaction`)
  - membership/state changes (`m.room.member`, room name/topic updates)
- Keep raw fallback text for unknown event shapes.
- Continue surfacing wrapper hints for undecrypted events.

## Notes

- Keep this client focused as a native test harness.
- Avoid login/account creation UI for now; assume account setup is handled elsewhere.
