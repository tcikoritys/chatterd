#!/usr/bin/env python3

import json
import socket
import threading
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Tuple

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gdk, Gtk


DEFAULT_ADDR = "127.0.0.1:9388"
DEFAULT_LIMIT = 50


class RpcError(Exception):
    pass


class RpcClient:
    def __init__(self, addr: str) -> None:
        self.addr = addr
        self._next_id = 1
        self._lock = threading.Lock()

    def set_addr(self, addr: str) -> None:
        self.addr = addr

    def call(self, method: str, params: Optional[Dict[str, Any]] = None) -> Any:
        with self._lock:
            req_id = self._next_id
            self._next_id += 1

        payload = {
            "jsonrpc": "2.0",
            "id": req_id,
            "method": method,
            "params": params,
        }

        try:
            with socket.create_connection(self._split_addr(self.addr), timeout=10.0) as sock:
                encoded = (json.dumps(payload) + "\n").encode("utf-8")
                sock.sendall(encoded)
                fileobj = sock.makefile("r", encoding="utf-8", newline="\n")
                line = fileobj.readline()
        except OSError as exc:
            raise RpcError(f"RPC connection failed: {exc}") from exc

        if not line:
            raise RpcError("RPC server closed connection without a response")

        try:
            msg = json.loads(line)
        except json.JSONDecodeError as exc:
            raise RpcError(f"Invalid JSON-RPC response: {exc}") from exc

        if msg.get("id") != req_id:
            raise RpcError("Mismatched JSON-RPC response id")

        if "error" in msg and msg["error"] is not None:
            err = msg["error"]
            code = err.get("code", "?") if isinstance(err, dict) else "?"
            text = err.get("message", str(err)) if isinstance(err, dict) else str(err)
            raise RpcError(f"RPC error {code}: {text}")

        return msg.get("result")

    @staticmethod
    def _split_addr(addr: str) -> Tuple[str, int]:
        parts = addr.rsplit(":", 1)
        if len(parts) != 2:
            raise RpcError(f"Address must look like host:port, got: {addr}")
        host = parts[0]
        try:
            port = int(parts[1])
        except ValueError as exc:
            raise RpcError(f"Invalid port in address: {addr}") from exc
        return host, port


class EventSubscriber:
    def __init__(
        self,
        addr: str,
        account_id: str,
        since: Optional[str],
        on_ready: Callable[[str, str], None],
        on_event: Callable[[Dict[str, Any]], None],
        on_error: Callable[[str], None],
    ) -> None:
        self.addr = addr
        self.account_id = account_id
        self.since = since
        self.on_ready = on_ready
        self.on_event = on_event
        self.on_error = on_error
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None

    def start(self) -> None:
        if self._thread is not None:
            return
        self._thread = threading.Thread(target=self._run, name="events-subscribe", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()

    def _run(self) -> None:
        request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "events.subscribe",
            "params": {
                "account_id": self.account_id,
                "since": self.since,
                "snapshot": True,
            },
        }

        try:
            with socket.create_connection(RpcClient._split_addr(self.addr), timeout=10.0) as sock:
                sock.settimeout(None)
                sock.sendall((json.dumps(request) + "\n").encode("utf-8"))
                fileobj = sock.makefile("r", encoding="utf-8", newline="\n")

                while not self._stop.is_set():
                    line = fileobj.readline()
                    if not line:
                        if not self._stop.is_set():
                            self.on_error("Event stream disconnected")
                        return

                    try:
                        msg = json.loads(line)
                    except json.JSONDecodeError:
                        continue

                    if msg.get("id") == 1:
                        error = msg.get("error")
                        if error is not None:
                            code = error.get("code", "?") if isinstance(error, dict) else "?"
                            text = (
                                error.get("message", str(error))
                                if isinstance(error, dict)
                                else str(error)
                            )
                            self.on_error(f"events.subscribe failed {code}: {text}")
                            return
                        result = msg.get("result") or {}
                        subscription_id = str(result.get("subscription_id", ""))
                        since = str(result.get("since", "cursor-0"))
                        self.on_ready(subscription_id, since)
                        continue

                    if msg.get("method") == "events.push":
                        params = msg.get("params")
                        if isinstance(params, dict):
                            self.on_event(params)

        except OSError as exc:
            if not self._stop.is_set():
                self.on_error(f"Event stream connection failed: {exc}")


@dataclass
class RoomRowData:
    room_id: str
    title: str


class ChatWindow(Gtk.ApplicationWindow):
    def __init__(self, app: Gtk.Application) -> None:
        super().__init__(application=app)
        self.set_title("chatterd GTK test client")
        self.set_default_size(980, 680)

        self.rpc = RpcClient(DEFAULT_ADDR)
        self.subscriber: Optional[EventSubscriber] = None
        self.account_id: Optional[str] = None
        self.subscription_id: Optional[str] = None
        self.since: Optional[str] = None
        self.selected_room_id: Optional[str] = None
        self.pending_room_id: Optional[str] = None
        self.pending_room_title: Optional[str] = None
        self.preferred_account_id: str = ""
        self.rooms: Dict[str, Dict[str, Any]] = {}

        self._build_ui()

    def _build_ui(self) -> None:
        root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        root.set_margin_top(8)
        root.set_margin_bottom(8)
        root.set_margin_start(8)
        root.set_margin_end(8)

        self.view_stack = Gtk.Stack()
        self.view_stack.set_hexpand(True)
        self.view_stack.set_vexpand(True)

        disconnected = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        disconnected.set_valign(Gtk.Align.CENTER)
        disconnected.set_halign(Gtk.Align.CENTER)

        disconnected_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.disconnected_addr_entry = Gtk.Entry()
        self.disconnected_addr_entry.set_hexpand(True)
        self.disconnected_addr_entry.set_text(DEFAULT_ADDR)
        self.disconnected_addr_entry.set_width_chars(28)

        self.connect_button = Gtk.Button(label="Connect...")
        self.connect_button.connect("clicked", self._on_connect_clicked)
        disconnected_row.append(Gtk.Label(label="Daemon"))
        disconnected_row.append(self.disconnected_addr_entry)
        disconnected_row.append(self.connect_button)

        disconnected.append(disconnected_row)
        self.view_stack.add_named(disconnected, "disconnected")

        connected = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        connected_controls = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.connection_button = Gtk.Button(label="Connection...")
        self.connection_button.connect("clicked", self._on_connect_clicked)
        self.reload_rooms_button = Gtk.Button(label="Reload Rooms")
        self.reload_rooms_button.connect("clicked", self._on_reload_rooms)
        self.disconnect_button = Gtk.Button(label="Disconnect")
        self.disconnect_button.connect("clicked", self._on_disconnect_clicked)
        self.account_label = Gtk.Label(label="Account: -")
        self.account_label.set_xalign(0.0)
        self.account_label.set_hexpand(True)

        connected_controls.append(self.connection_button)
        connected_controls.append(self.reload_rooms_button)
        connected_controls.append(self.disconnect_button)
        connected_controls.append(self.account_label)

        paned = Gtk.Paned.new(Gtk.Orientation.HORIZONTAL)
        paned.set_wide_handle(True)

        left_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        left_box.append(Gtk.Label(label="Rooms"))

        self.room_list = Gtk.ListBox()
        self.room_list.set_selection_mode(Gtk.SelectionMode.SINGLE)
        self.room_list.set_activate_on_single_click(False)
        self.room_list.set_focusable(True)
        self.room_list.connect("row-selected", self._on_room_selected)
        key_controller = Gtk.EventControllerKey.new()
        key_controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        key_controller.connect("key-pressed", self._on_room_list_key_pressed)
        self.room_list.add_controller(key_controller)

        room_scroll = Gtk.ScrolledWindow()
        room_scroll.set_hexpand(True)
        room_scroll.set_vexpand(True)
        room_scroll.set_child(self.room_list)
        left_box.append(room_scroll)

        right_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.room_header = Gtk.Label(label="No room selected")
        self.room_header.set_xalign(0.0)
        right_box.append(self.room_header)

        self.message_list = Gtk.ListBox()
        self.message_list.set_selection_mode(Gtk.SelectionMode.SINGLE)
        self.message_list.set_activate_on_single_click(False)
        self.message_list.set_focusable(True)
        msg_key_controller = Gtk.EventControllerKey.new()
        msg_key_controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        msg_key_controller.connect("key-pressed", self._on_message_list_key_pressed)
        self.message_list.add_controller(msg_key_controller)

        self.message_scroll = Gtk.ScrolledWindow()
        self.message_scroll.set_hexpand(True)
        self.message_scroll.set_vexpand(True)
        self.message_scroll.set_child(self.message_list)
        right_box.append(self.message_scroll)

        send_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.message_entry = Gtk.Entry()
        self.message_entry.set_hexpand(True)
        self.message_entry.connect("activate", self._on_send_clicked)
        entry_key_controller = Gtk.EventControllerKey.new()
        entry_key_controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        entry_key_controller.connect("key-pressed", self._on_message_entry_key_pressed)
        self.message_entry.add_controller(entry_key_controller)

        send_row.append(self.message_entry)
        right_box.append(send_row)

        paned.set_start_child(left_box)
        paned.set_end_child(right_box)
        paned.set_position(320)
        connected.append(connected_controls)
        connected.append(paned)
        self.view_stack.add_named(connected, "connected")
        self.view_stack.set_visible_child_name("disconnected")

        self.status_label = Gtk.Label(label="Disconnected")
        self.status_label.set_xalign(0.0)

        root.append(self.view_stack)
        root.append(self.status_label)

        self.set_child(root)
        win_keys = Gtk.EventControllerKey.new()
        win_keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        win_keys.connect("key-pressed", self._on_window_key_pressed)
        self.add_controller(win_keys)

    def _set_status(self, text: str) -> None:
        self.status_label.set_text(text)

    def _on_connect_clicked(self, _button: Gtk.Button) -> None:
        initial_addr = self.rpc.addr
        if self.view_stack.get_visible_child_name() == "disconnected":
            initial_addr = self.disconnected_addr_entry.get_text().strip() or self.rpc.addr
        initial_account = self.account_id or self.preferred_account_id
        self._open_connect_dialog(initial_addr, initial_account)

    def _open_connect_dialog(self, initial_addr: str, initial_account: str) -> None:
        dialog = Gtk.Dialog(title="Connect", transient_for=self, modal=True)
        dialog.set_default_size(480, 1)
        dialog.add_button("Cancel", Gtk.ResponseType.CANCEL)
        dialog.add_button("Connect", Gtk.ResponseType.OK)
        dialog.set_default_response(Gtk.ResponseType.OK)

        content = dialog.get_content_area()
        content.set_spacing(8)
        content.set_margin_top(12)
        content.set_margin_bottom(12)
        content.set_margin_start(12)
        content.set_margin_end(12)

        addr_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        addr_label = Gtk.Label(label="Daemon")
        addr_label.set_xalign(0.0)
        addr_entry = Gtk.Entry()
        addr_entry.set_hexpand(True)
        addr_entry.set_text(initial_addr or DEFAULT_ADDR)
        addr_row.append(addr_label)
        addr_row.append(addr_entry)

        account_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        account_label = Gtk.Label(label="Account")
        account_label.set_xalign(0.0)
        account_entry = Gtk.Entry()
        account_entry.set_hexpand(True)
        account_entry.set_placeholder_text("account_id (optional)")
        account_entry.set_text(initial_account or "")
        account_row.append(account_label)
        account_row.append(account_entry)

        content.append(addr_row)
        content.append(account_row)

        def on_response(dlg: Gtk.Dialog, response: int) -> None:
            if response == Gtk.ResponseType.OK:
                addr = addr_entry.get_text().strip() or DEFAULT_ADDR
                account = account_entry.get_text().strip()
                self._start_connect(addr, account)
            dlg.destroy()

        dialog.connect("response", on_response)
        dialog.present()

    def _start_connect(self, addr: str, preferred_account: str) -> None:
        self._set_status("Connecting...")
        self._disconnect_subscriber()
        self.rpc.set_addr(addr)
        self.disconnected_addr_entry.set_text(addr)
        self.preferred_account_id = preferred_account

        def worker() -> Dict[str, Any]:
            ping = self.rpc.call("rpc.ping")
            accounts = self.rpc.call("account.list")
            if not isinstance(accounts, list):
                raise RpcError("account.list returned an invalid response")

            chosen: Optional[str] = None

            if preferred_account:
                for item in accounts:
                    if isinstance(item, dict) and item.get("id") == preferred_account:
                        status = str(item.get("status", ""))
                        if status != "ready":
                            raise RpcError(
                                f"Account {preferred_account} is not ready (status={status})"
                            )
                        chosen = preferred_account
                        break
                if chosen is None:
                    raise RpcError(f"Account not found: {preferred_account}")
            else:
                for item in accounts:
                    if isinstance(item, dict) and str(item.get("status", "")) == "ready":
                        chosen = str(item.get("id", ""))
                        break
                if not chosen:
                    raise RpcError("No ready account found")

            rooms = self.rpc.call("room.list", {"account_id": chosen})
            return {
                "ping": ping,
                "account_id": chosen,
                "rooms": rooms,
            }

        self._run_worker(worker, self._on_connect_success)

    def _on_connect_success(self, result: Dict[str, Any]) -> None:
        self.account_id = result["account_id"]
        self.preferred_account_id = self.account_id
        self.account_label.set_text(f"Account: {self.account_id}")
        self.view_stack.set_visible_child_name("connected")

        rooms_payload = result.get("rooms")
        rooms = []
        if isinstance(rooms_payload, dict):
            rooms = rooms_payload.get("rooms", []) or []

        self._set_rooms(rooms)
        self._subscribe_events()
        self._set_status(f"Connected (pong={result['ping']}) account={self.account_id}")

    def _on_reload_rooms(self, _button: Gtk.Button) -> None:
        if not self.account_id:
            self._set_status("Connect first")
            return

        account_id = self.account_id

        def worker() -> Any:
            return self.rpc.call("room.list", {"account_id": account_id})

        def done(payload: Any) -> None:
            rooms = []
            if isinstance(payload, dict):
                rooms = payload.get("rooms", []) or []
            self._set_rooms(rooms)
            self._set_status(f"Reloaded rooms ({len(rooms)})")

        self._run_worker(worker, done)

    def _on_disconnect_clicked(self, _button: Gtk.Button) -> None:
        self._disconnect_subscriber()
        self.account_id = None
        self.selected_room_id = None
        self.pending_room_id = None
        self.pending_room_title = None
        self.since = None
        self.account_label.set_text("Account: -")
        self.room_header.set_text("No room selected")
        self._set_rooms([])
        self._set_messages([])
        self.view_stack.set_visible_child_name("disconnected")
        self._set_status("Disconnected")

    def _subscribe_events(self) -> None:
        if not self.account_id:
            return

        self._disconnect_subscriber()

        def on_ready(subscription_id: str, since: str) -> None:
            GLib.idle_add(self._on_subscriber_ready, subscription_id, since)

        def on_event(params: Dict[str, Any]) -> None:
            GLib.idle_add(self._handle_event_notification, params)

        def on_error(text: str) -> None:
            GLib.idle_add(self._set_status, text)

        self.subscriber = EventSubscriber(
            addr=self.rpc.addr,
            account_id=self.account_id,
            since=self.since,
            on_ready=on_ready,
            on_event=on_event,
            on_error=on_error,
        )
        self.subscriber.start()

    def _on_subscriber_ready(self, subscription_id: str, since: str) -> bool:
        self.subscription_id = subscription_id
        self.since = since
        self._set_status(
            f"Subscribed events: subscription_id={subscription_id} since={since}"
        )
        return False

    def _disconnect_subscriber(self) -> None:
        if self.subscriber is not None:
            self.subscriber.stop()
            self.subscriber = None
        self.subscription_id = None

    def _handle_event_notification(self, params: Dict[str, Any]) -> bool:
        cursor = params.get("cursor")
        if isinstance(cursor, str):
            self.since = cursor

        event_type = params.get("type")
        data = params.get("data")

        if event_type == "room.list.snapshot" and isinstance(data, dict):
            rooms = data.get("rooms", [])
            if isinstance(rooms, list):
                self._set_rooms(rooms)
                self._set_status(f"Snapshot rooms: {len(rooms)}")

        if event_type == "room.message" and isinstance(data, dict):
            room_id = data.get("room_id")
            if isinstance(room_id, str) and room_id == self.selected_room_id:
                text = self._format_message_line(data)
                self._append_message(text)

        if event_type == "events.reset" and isinstance(data, dict):
            new_since = data.get("new_since")
            if isinstance(new_since, str):
                self.since = new_since
            reason = data.get("reason", "unknown")
            self._set_status(f"events.reset ({reason}), cursor={self.since}")
            self._refresh_current_room_messages()

        return False

    def _on_room_selected(
        self, _listbox: Gtk.ListBox, row: Optional[Gtk.ListBoxRow]
    ) -> None:
        if row is None:
            return

        data = getattr(row, "_room_data", None)
        if not isinstance(data, RoomRowData):
            return

        self.pending_room_id = data.room_id
        self.pending_room_title = data.title
        if self.selected_room_id == data.room_id:
            self.room_header.set_text(f"{data.title} ({data.room_id})")
        else:
            self.room_header.set_text(
                f"{data.title} ({data.room_id}) - press Enter to open"
            )

    def _on_room_list_key_pressed(
        self,
        _controller: Gtk.EventControllerKey,
        keyval: int,
        _keycode: int,
        _state: int,
    ) -> bool:
        if keyval in (Gdk.KEY_Return, Gdk.KEY_KP_Enter):
            self._open_pending_room()
            return True
        return False

    def _on_window_key_pressed(
        self,
        _controller: Gtk.EventControllerKey,
        keyval: int,
        _keycode: int,
        state: int,
    ) -> bool:
        if keyval not in (Gdk.KEY_Tab, Gdk.KEY_ISO_Left_Tab):
            return False

        focus = self.get_focus()
        if focus is None:
            return False

        in_room_list = False
        if focus is self.room_list:
            in_room_list = True
        else:
            ancestor = focus.get_ancestor(Gtk.ListBox)
            in_room_list = ancestor is self.room_list

        if in_room_list:
            shift = keyval == Gdk.KEY_ISO_Left_Tab or bool(
                state & int(Gdk.ModifierType.SHIFT_MASK)
            )
            if shift:
                self.reload_rooms_button.grab_focus()
            else:
                self.message_entry.grab_focus()
            return True

        in_message_list = False
        if focus is self.message_list:
            in_message_list = True
        else:
            ancestor = focus.get_ancestor(Gtk.ListBox)
            in_message_list = ancestor is self.message_list

        if in_message_list:
            shift = keyval == Gdk.KEY_ISO_Left_Tab or bool(
                state & int(Gdk.ModifierType.SHIFT_MASK)
            )
            if shift:
                self.room_list.grab_focus()
            else:
                self.message_entry.grab_focus()
            return True

        return False

    def _on_message_list_key_pressed(
        self,
        _controller: Gtk.EventControllerKey,
        keyval: int,
        _keycode: int,
        state: int,
    ) -> bool:
        if keyval not in (Gdk.KEY_Tab, Gdk.KEY_ISO_Left_Tab):
            return False
        shift = keyval == Gdk.KEY_ISO_Left_Tab or bool(
            state & int(Gdk.ModifierType.SHIFT_MASK)
        )
        if shift:
            self.room_list.grab_focus()
        else:
            self.message_entry.grab_focus()
        return True

    def _on_message_entry_key_pressed(
        self,
        _controller: Gtk.EventControllerKey,
        keyval: int,
        _keycode: int,
        state: int,
    ) -> bool:
        if keyval not in (Gdk.KEY_Tab, Gdk.KEY_ISO_Left_Tab):
            return False
        shift = keyval == Gdk.KEY_ISO_Left_Tab or bool(
            state & int(Gdk.ModifierType.SHIFT_MASK)
        )
        if shift:
            self.room_list.grab_focus()
        else:
            self.message_list.grab_focus()
        return True

    def _open_pending_room(self) -> None:
        if not self.pending_room_id or not self.pending_room_title:
            return
        if self.selected_room_id == self.pending_room_id:
            return
        self.selected_room_id = self.pending_room_id
        self.room_header.set_text(
            f"{self.pending_room_title} ({self.pending_room_id})"
        )
        self._fetch_room_messages(self.pending_room_id)

    def _fetch_room_messages(self, room_id: str) -> None:
        if not self.account_id:
            return

        account_id = self.account_id

        def worker() -> Any:
            return self.rpc.call(
                "room.messages",
                {
                    "account_id": account_id,
                    "room_id": room_id,
                    "limit": DEFAULT_LIMIT,
                },
            )

        def done(payload: Any) -> None:
            chunk = []
            if isinstance(payload, dict):
                chunk = payload.get("chunk", []) or []
            lines: List[str] = []
            for item in reversed(chunk):
                lines.append(self._format_message_line(item))
            self._set_messages(lines)
            self._set_status(f"Loaded messages for {room_id} ({len(lines)})")

        self._run_worker(worker, done)

    def _refresh_current_room_messages(self) -> None:
        if self.selected_room_id:
            self._fetch_room_messages(self.selected_room_id)

    def _on_send_clicked(self, _button: Gtk.Widget) -> None:
        if not self.account_id:
            self._set_status("Connect first")
            return
        if not self.selected_room_id:
            self._set_status("Select a room first")
            return

        body = self.message_entry.get_text()
        if not body.strip():
            return

        account_id = self.account_id
        room_id = self.selected_room_id

        def worker() -> Any:
            return self.rpc.call(
                "room.send",
                {
                    "account_id": account_id,
                    "room_id": room_id,
                    "body": body,
                },
            )

        def done(_result: Any) -> None:
            self.message_entry.set_text("")
            self._set_status("Message sent; waiting for room.message event")

        self._run_worker(worker, done)

    def _run_worker(self, fn: Callable[[], Any], on_done: Callable[[Any], None]) -> None:
        def job() -> None:
            try:
                result = fn()
            except Exception as exc:
                GLib.idle_add(self._set_status, f"Error: {exc}")
                return
            GLib.idle_add(self._complete_job, on_done, result)

        threading.Thread(target=job, daemon=True).start()

    def _complete_job(self, on_done: Callable[[Any], None], result: Any) -> bool:
        on_done(result)
        return False

    def _set_rooms(self, rooms: List[Any]) -> None:
        self.rooms.clear()

        child = self.room_list.get_first_child()
        while child is not None:
            nxt = child.get_next_sibling()
            self.room_list.remove(child)
            child = nxt

        for room in rooms:
            if not isinstance(room, dict):
                continue
            room_id = room.get("room_id")
            if not isinstance(room_id, str):
                continue
            name = room.get("name")
            if not isinstance(name, str) or not name:
                name = "unnamed"

            self.rooms[room_id] = room
            row = Gtk.ListBoxRow()
            row_data = RoomRowData(room_id=room_id, title=name)
            setattr(row, "_room_data", row_data)

            row_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
            row_box.set_margin_top(6)
            row_box.set_margin_bottom(6)
            row_box.set_margin_start(6)
            row_box.set_margin_end(6)
            name_label = Gtk.Label(label=name)
            name_label.set_xalign(0.0)
            row_box.append(name_label)

            row.set_child(row_box)
            self.room_list.append(row)

    def _set_messages(self, lines: List[str]) -> None:
        child = self.message_list.get_first_child()
        while child is not None:
            nxt = child.get_next_sibling()
            self.message_list.remove(child)
            child = nxt

        for line in lines:
            self._append_message(line)
        first = self.message_list.get_first_child()
        if isinstance(first, Gtk.ListBoxRow):
            self.message_list.select_row(first)

    def _append_message(self, line: str) -> None:
        row = Gtk.ListBoxRow()

        label = Gtk.Label(label=line)
        label.set_wrap(True)
        label.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        label.set_xalign(0.0)
        label.set_selectable(False)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0)
        box.set_margin_top(4)
        box.set_margin_bottom(4)
        box.set_margin_start(6)
        box.set_margin_end(6)
        box.append(label)

        row.set_child(box)
        self.message_list.append(row)

        adj = self.message_scroll.get_vadjustment()
        if adj is not None:
            GLib.idle_add(adj.set_value, adj.get_upper())

    @staticmethod
    def _extract_event_object(item: Any) -> Optional[Dict[str, Any]]:
        if not isinstance(item, dict):
            return None
        event = item.get("event")
        if isinstance(event, dict):
            return event

        kind = item.get("kind")
        if isinstance(kind, dict):
            for wrapped in kind.values():
                if isinstance(wrapped, dict):
                    event = wrapped.get("event")
                    if isinstance(event, dict):
                        return event

        return item

    def _format_message_line(self, item: Any) -> str:
        event = self._extract_event_object(item)
        if not isinstance(event, dict):
            return "<invalid event>"

        sender = event.get("sender_display_name")
        if not isinstance(sender, str) or not sender:
            sender = event.get("sender")
        if not isinstance(sender, str) or not sender:
            sender = "unknown"

        content = event.get("content")
        body: Optional[str] = None
        msgtype = ""
        if isinstance(content, dict):
            raw_body = content.get("body")
            if isinstance(raw_body, str):
                body = raw_body
            raw_msgtype = content.get("msgtype")
            if isinstance(raw_msgtype, str):
                msgtype = raw_msgtype

        event_type = event.get("type")
        if not isinstance(event_type, str) and isinstance(item, dict):
            top_type = item.get("type")
            if isinstance(top_type, str):
                event_type = top_type
        if not isinstance(event_type, str) and isinstance(item, dict):
            kind = item.get("kind")
            if isinstance(kind, dict) and len(kind) == 1:
                only_key = next(iter(kind.keys()))
                if isinstance(only_key, str) and only_key:
                    event_type = f"kind.{only_key}"
        if not isinstance(event_type, str):
            event_type = "unknown"

        if msgtype in {"m.text", "m.notice", "m.emote"} and body is not None:
            return f"{sender}: {body}"

        if event_type == "m.room.message" and body is not None:
            return f"{sender}: {body}"

        if event_type == "kind.UnableToDecrypt":
            if isinstance(content, dict) and content:
                keys = ",".join(sorted(content.keys())[:4])
                return f"<undecrypted event (waiting for keys), content_keys={keys}>"
            return "<undecrypted event (waiting for keys)>"

        if msgtype:
            return f"<non-text event: {event_type}, msgtype={msgtype}>"
        if isinstance(content, dict) and content:
            keys = ",".join(sorted(content.keys())[:4])
            return f"<non-text event: {event_type}, content_keys={keys}>"
        return f"<non-text event: {event_type}>"


class ChatApp(Gtk.Application):
    def __init__(self) -> None:
        super().__init__(application_id="org.chatter.testgtk")

    def do_activate(self) -> None:
        win = self.props.active_window
        if win is None:
            win = ChatWindow(self)
        win.present()


def main() -> None:
    app = ChatApp()
    app.run(None)


if __name__ == "__main__":
    main()
