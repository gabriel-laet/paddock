#!/usr/bin/env python3
"""Shared helpers for in-tree paddock exec plugins. Import via sys.path; no install."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from datetime import date, timedelta
from typing import Any

EMAIL_RE = re.compile(
    r"([A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,})",
    re.IGNORECASE,
)
FROM_RE = re.compile(r'^\s*"?([^"<]*?)"?\s*<([^>]+)>')
TOPIC_RE = re.compile(r"/topics/([^/?#]+)")


def env_bin(key: str, default: str) -> str:
    v = os.environ.get(key, "").strip()
    return v or default


def gog_bin() -> str:
    return env_bin("PADDOCK_GOG", "gog")


def hey_bin() -> str:
    return env_bin("PADDOCK_HEY", "hey")


def wacli_bin() -> str:
    return env_bin("PADDOCK_WACLI", "wacli")


def redact(text: str | None) -> str:
    if not text:
        return ""
    out = text
    pw = os.environ.get("GOG_KEYRING_PASSWORD")
    if pw:
        out = out.replace(pw, "****")
    return out


def die(msg: str, code: int = 1) -> None:
    sys.stderr.write(redact(msg).rstrip() + "\n")
    raise SystemExit(code)


def cannot_send() -> None:
    die("source cannot send", 2)


def emit_items(items: list[dict[str, Any]]) -> None:
    json.dump(items, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


def emit_send(foreign_id: str, start: str | None = None, end: str | None = None) -> None:
    out: dict[str, Any] = {"foreign_id": foreign_id}
    if start:
        out["start"] = start
    if end:
        out["end"] = end
    json.dump(out, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


def read_draft() -> dict[str, Any]:
    raw = sys.stdin.read()
    if not raw.strip():
        return {}
    data = json.loads(raw)
    return data if isinstance(data, dict) else {}


def verb() -> str:
    if len(sys.argv) < 2:
        return ""
    return sys.argv[-1].strip()


def fixture_path() -> str | None:
    p = os.environ.get("PADDOCK_PLUGIN_FIXTURE", "").strip()
    return p or None


def load_fixture() -> Any | None:
    path = fixture_path()
    if not path:
        return None
    with open(path, encoding="utf-8") as f:
        text = f.read()
    if not text.strip():
        return {}
    return json.loads(text)


def parse_json_stdout(text: str) -> Any:
    text = text.strip()
    if not text:
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        for i, ch in enumerate(text):
            if ch in "[{":
                try:
                    return json.loads(text[i:])
                except json.JSONDecodeError:
                    continue
        raise


def unwrap_list(raw: Any, *names: str) -> list[Any]:
    if raw is None:
        return []
    if isinstance(raw, list):
        return raw
    if not isinstance(raw, dict):
        return []
    for n in names:
        if n in raw:
            v = raw[n]
            if isinstance(v, list):
                return v
            if v is None:
                continue
            inner = unwrap_list(v, *names)
            if inner:
                return inner
    for n in ("data", "result", "results"):
        if n in raw:
            inner = unwrap_list(raw[n], *names)
            if inner or raw[n] in ([], {}):
                return inner
    return []


def pick(obj: dict[str, Any], *keys: str) -> Any:
    for k in keys:
        if k in obj and obj[k] not in (None, ""):
            return obj[k]
    lower = {str(k).lower(): v for k, v in obj.items()}
    for k in keys:
        v = lower.get(k.lower())
        if v not in (None, ""):
            return v
    return None


def actor(aid: str, name: str | None = None, kind: str = "person") -> dict[str, Any]:
    out: dict[str, Any] = {"id": aid, "kind": kind}
    if name:
        out["name"] = name
    return out


def parse_from_header(header: str | None) -> dict[str, Any] | None:
    if not header or not str(header).strip():
        return None
    s = str(header).strip()
    m = FROM_RE.match(s)
    if m:
        name = m.group(1).strip().strip("\"") or None
        email = m.group(2).strip()
        em = EMAIL_RE.search(email)
        return actor(em.group(1) if em else email, name, "person")
    em = EMAIL_RE.search(s)
    if em:
        return actor(em.group(1), None, "person")
    return actor(s, None, "person")


def looks_like_list(email: Any, name: Any) -> bool:
    blob = f"{email or ''} {name or ''}".lower()
    return "list" in blob or "+list@" in blob


def email_actor(obj: Any, *, kind: str = "person") -> dict[str, Any] | None:
    if obj is None:
        return None
    if isinstance(obj, str):
        return parse_from_header(obj)
    if not isinstance(obj, dict):
        return None
    email = pick(obj, "email", "email_address", "emailAddress", "address", "id")
    name = pick(obj, "displayName", "display_name", "name")
    if isinstance(name, str):
        name = name.strip() or None
    else:
        name = None
    kind_hint = pick(obj, "kind", "type")
    if isinstance(kind_hint, str) and kind_hint.lower() in ("group", "list", "person"):
        kind = kind_hint.lower()
    elif looks_like_list(email, name):
        kind = "list"
    if not email:
        return actor(str(name), name, kind) if name else None
    em = EMAIL_RE.search(str(email))
    return actor(em.group(1) if em else str(email), name, kind)


def header_map(headers: Any) -> dict[str, str]:
    out: dict[str, str] = {}
    if not isinstance(headers, list):
        return out
    for h in headers:
        if not isinstance(h, dict):
            continue
        name = str(h.get("name") or "").strip()
        val = h.get("value")
        if name and val is not None:
            out[name.lower()] = str(val)
    return out


def event_instant(node: Any) -> str | None:
    if node is None:
        return None
    if isinstance(node, str):
        s = node.strip()
        return s or None
    if isinstance(node, dict):
        v = pick(node, "dateTime", "date_time", "date")
        if isinstance(v, str) and v.strip():
            return v.strip()
    return None


def last_id(fid: str | None) -> str:
    if not fid:
        return ""
    s = str(fid).strip()
    if "/" in s:
        return s.rsplit("/", 1)[-1]
    return s


def topic_from_url(url: str | None) -> str:
    if not url:
        return ""
    m = TOPIC_RE.search(str(url))
    return m.group(1) if m else ""


def is_hey_event(obj: dict[str, Any]) -> bool:
    type_keys = (
        "type",
        "kind",
        "_type",
        "class",
        "recordable_type",
        "recording_type",
        "key",
    )
    saw = False
    for k in type_keys:
        v = obj.get(k)
        if isinstance(v, str) and v.strip():
            saw = True
            if "Event" in v:
                return True
    if saw:
        return False
    return bool(pick(obj, "starts_at", "startsAt", "start"))


def media_part(
    path: str,
    mime: str | None,
    media_type: str | None,
    filename: str | None,
) -> dict[str, Any]:
    mime = (mime or "").strip()
    mt = (media_type or "").strip().lower()
    kind = "file"
    if mime.startswith("image/") or mt in ("image", "sticker", "photo"):
        kind = "image"
    elif mime.startswith("audio/") or mt in ("audio", "ptt", "voice"):
        kind = "audio"
    elif mime.startswith("video/") or mt in ("video",):
        kind = "video"
    part: dict[str, Any] = {
        "kind": kind,
        "mime": mime or "application/octet-stream",
        "path": path,
    }
    if filename:
        part["text"] = filename
    return part


def today_window(days: int) -> tuple[str, str]:
    start = date.today()
    end = start + timedelta(days=days)
    return start.isoformat(), end.isoformat()


def _unknown_flag(stderr: str, stdout: str) -> bool:
    blob = f"{stderr}\n{stdout}".lower()
    return any(
        s in blob
        for s in (
            "unknown flag",
            "unknown option",
            "unrecognized",
            "no such flag",
            "not defined",
            "unexpected argument",
        )
    )


def run_cli(argv: list[str]) -> subprocess.CompletedProcess[str]:
    # Inherit env (including GOG_KEYRING_PASSWORD). Never log it.
    return subprocess.run(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )


def parse_json_stdout_or_die(text: str, name: str) -> Any:
    try:
        return parse_json_stdout(text)
    except json.JSONDecodeError:
        die(f"{name}: invalid JSON", 1)
    return {}


def run_json(argv: list[str], *, allow_fail: bool = False) -> Any:
    p = run_cli(argv)
    if p.returncode != 0 and not allow_fail:
        detail = redact(p.stderr.strip() or p.stdout.strip() or f"exit {p.returncode}")
        die(f"{argv[0]} failed: {detail}", p.returncode or 1)
    if not p.stdout.strip():
        return {}
    try:
        return parse_json_stdout(p.stdout)
    except json.JSONDecodeError:
        if allow_fail or p.returncode != 0:
            return {}
        die(f"{argv[0]}: invalid JSON", 1)
    return {}


def gog_run(args: list[str], *, write: bool = False) -> Any:
    binary = gog_bin()
    if write:
        flag_sets = [["--no-input", "--json"]]
    else:
        flag_sets = [
            ["--readonly", "--no-input", "--json", "--results-only"],
            ["--no-input", "--json", "--results-only"],
            ["--no-input", "--json"],
        ]
    last_err = "gog failed"
    last_code = 1
    for flags in flag_sets:
        argv = [binary] + flags + args
        p = run_cli(argv)
        if p.returncode == 0:
            return parse_json_stdout_or_die(p.stdout, "gog")
        last_err = redact(p.stderr.strip() or p.stdout.strip() or f"exit {p.returncode}")
        last_code = p.returncode or 1
        if not _unknown_flag(p.stderr, p.stdout):
            break
    die(f"gog failed: {last_err}", last_code)
    return {}


def gog_accounts() -> list[str]:
    one = (
        os.environ.get("GOG_ACCOUNT", "").strip()
        or os.environ.get("PADDOCK_GOG_ACCOUNT", "").strip()
    )
    if one:
        return [one]
    fx = load_fixture()
    if fx is not None:
        if isinstance(fx, dict):
            acct = fx.get("account") or fx.get("email")
            if isinstance(acct, str) and acct.strip():
                return [acct.strip()]
        return ["me"]
    try:
        raw = gog_run(["auth", "list"], write=True)
    except SystemExit:
        return ["me"]
    emails: list[str] = []
    rows = unwrap_list(raw, "accounts")
    for a in rows:
        if isinstance(a, str) and a.strip():
            emails.append(a.strip())
        elif isinstance(a, dict):
            e = pick(a, "email", "account", "id")
            if e:
                emails.append(str(e).strip())
    return emails or ["me"]


def hey_run(args: list[str], *, allow_fail: bool = False) -> Any:
    return run_json([hey_bin()] + args, allow_fail=allow_fail)


def wacli_run(args: list[str], *, allow_fail: bool = False) -> Any:
    raw = run_json([wacli_bin()] + args, allow_fail=allow_fail)
    return wacli_payload(raw, allow_fail=allow_fail)


def wacli_payload(raw: Any, *, allow_fail: bool = False) -> Any:
    if isinstance(raw, dict) and ("success" in raw or "data" in raw or "error" in raw):
        if raw.get("success") is False and not allow_fail:
            die(str(raw.get("error") or "wacli failed"), 1)
        if "data" in raw:
            return raw.get("data")
    return raw


def pick_foreign_id(raw: Any, *extra: str) -> str:
    keys = extra + (
        "foreign_id",
        "id",
        "messageId",
        "message_id",
        "MsgID",
        "threadId",
        "thread_id",
        "topic_id",
    )
    if isinstance(raw, dict):
        for k in keys:
            v = raw.get(k)
            if v not in (None, ""):
                return str(v)
        got = pick(raw, *keys)
        if got not in (None, ""):
            return str(got)
        for nest in ("data", "result", "message", "event"):
            inner = raw.get(nest)
            if isinstance(inner, dict):
                found = pick_foreign_id(inner, *extra)
                if found:
                    return found
    return ""
