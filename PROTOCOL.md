# The exec protocol

How paddock talks to a plugin. A plugin is a program; the host runs it with a
verb as the last argument and a JSON request on stdin. Any language works.
Rust plugins use the `paddock-protocol` crate, which holds these types and
nothing else of paddock's.

```
{cmd} {args...} pull     stdin: request           stdout: [item, ...] or one item per line
{cmd} {args...} send     stdin: request + draft   stdout: {foreign_id, start?, end?}
```

A source that cannot send exits 2, or prints `source cannot send`. Any other
non-zero exit is a failure the host reports with stderr.

## discovery

```toml
[[source]]
id = "feed"
kind = "rss"                # not a built-in kind, so: `paddock-rss` on PATH
url = "https://example.com/feed.xml"

[[source]]
id = "mine"
kind = "exec"               # an explicit program
cmd = "~/bin/my-source"
args = ["--account", "work"]
```

Built-in kinds are `fs` and `exec`. Any other `kind = "x"` runs `paddock-x`
found on `PATH`. Every key on the source block beyond `id`, `kind`, `name`,
and `forget_after` is handed to the plugin as its settings.

## request

```json
{ "id": "feed", "settings": { "url": "https://example.com/feed.xml" }, "draft": { ... } }
```

`draft` is present only for `send`. A plugin that reads no stdin is fine.

## item

Only `foreign_id` is required. Unknown fields are ignored.

```json
{
  "foreign_id": "msg-123",
  "title": "Invoice",
  "body": "the text, or a preview when parts carry the text",
  "href": "https://...",
  "start": "2026-09-14T10:00:00Z",
  "end": null,
  "thread": "conv-9",
  "from": { "id": "ana@example.com", "name": "Ana", "kind": "person" },
  "to": [ { "id": "fam@g.us", "name": "Family", "kind": "group" } ],
  "cites": [
    { "kind": "reply", "foreign_id": "msg-120", "excerpt": "quoted text" },
    { "kind": "mention", "actor": { "id": "bo" } },
    { "kind": "attach", "href": "/tmp/deck.pdf" }
  ],
  "parts": [
    { "kind": "text", "mime": "text/plain", "text": "the full text" },
    { "kind": "image", "mime": "image/png", "path": "/tmp/shot.png" }
  ],
  "read": false
}
```

- `foreign_id` is the source's stable name for the item. Admit upserts on
  `(source id, foreign_id)`; re-admit refreshes what the source sent and keeps
  read state and labels.
- `start` and `end` are RFC3339, or a bare `YYYY-MM-DD`. A message's own time
  goes in `start`; an event uses both.
- `thread` is the source's own grouping key. Without one, the host joins
  items by reply and forward cites.
- actor `kind` is `person` (default), `group`, `list`, or `agent`.
- cite `kind` is `reply`, `forward`, `quote`, `mention`, or `attach`. A cite
  names an item of the same source by `foreign_id` (or of another by
  `source_id` too), resolved when that item is in the pile, early or late; or
  something outside the pile by `href`. `excerpt` and `actor` are optional.
- part `kind` is `text`, `file`, `image`, `audio`, or `video`. Text is inline
  in `text`; anything else names a file in `path` that the host reads into
  its store.
- `read` is the source's opinion. Omit it when the source has none. A hand's
  unread wins over it.

## draft and sent

```json
{ "title": "re: Invoice", "body": "paid", "thread": "conv-9",
  "reply_to_foreign": "msg-123", "to": [ { "id": "ana@example.com" } ], "parts": [] }
```

```json
{ "foreign_id": "msg-124", "start": null, "end": null }
```

## message

Most sources are "someone sent something to someone". The `paddock-protocol`
crate has a `Message` shape that lowers to an item one way, so mail and chat
plugins make the same choices: the room (a chat, a channel, a list) is the
thread and a `to` actor of that kind; `reply_to` is a reply cite; mentions
are mention cites carrying the actor; attachments are file parts; `at` is
`start`; `seen` is `read`.

## checking a plugin

```
paddock check ./target/debug/paddock-rss --set url=https://example.com/feed.xml
paddock check paddock-mine --send
```

Runs `pull` (and `send` with `--send`), parses what comes back, and lists
anything the host would reject. This is the only thing in the paddock
repository that runs a plugin; the kernel's tests never do.
