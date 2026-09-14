# paddock

An inbox kernel with a CLI. No UI. Meant to be driven by hand, by scripts, and by agents.

The kernel is pure: four nouns, the questions inboxes ask, and the verbs. Everything that touches the world (SQLite, files, feeds, programs, chat models, the TOML config) is an adapter behind a port.

```
src/kernel/     item, inbox + question, classify (regex), verbs, ports
src/adapters/   sqlite, sources (fs, rss, exec), script (CEL), llm, host (paths, toml)
src/main.rs     the CLI
```

## nouns

- **item** — one thing that arrived, stripped of its source's shape
- **source** — a program that admits items (and may send)
- **label** — a mark a classifier or a hand put on an item
- **inbox** — a named question over the pile (sources + labels + time), not an account and not a folder

Items may have parts (text, file, image, audio, video) and an optional thread. `body` is the preview; the full text is the first text part.

Actors and cites are kernel: an item can have `from` / `to` (a person, a group, or a list) and may cite another item. A reply cites an item (`in_reply_to` + thread).

Inboxes nest. A child is a tighter question over its parent's matched items. Classifiers are owned by an inbox; they run when an item enters that inbox, then children re-evaluate. A label change re-runs classify (classify-on-enter) so a newly matching child can fire. `all/todo` is the todo list: same machinery, no extra feature.

## install

```
cargo install --path .
```

## commands

```
paddock init [--here]              # XDG, or ./.paddock
paddock pull                       # pull sources, classify new items, forget stale
paddock inboxes                    # tree with unread/total
paddock ls [INBOX] [--unread]      # items in an inbox path (default all)
paddock show ID                    # one item in full
paddock thread ID                  # every item in the same thread
paddock label ID [+l|-l]...        # add / remove labels, then reclassify
paddock read ID | unread ID
paddock forget ID                  # delete
paddock classify ID                # re-run classifiers
paddock why ID [INBOX]             # labels matched, classifiers that fired
paddock send [--title T] [--reply ID] [--to A]... [--source S] [BODY]   # body from arg or stdin
paddock context                    # dump this host for an agent
```

`--json` on any command prints machine output. `--remote[=HOST]` re-runs the same command over ssh (host from the flag, `PADDOCK_REMOTE`, or `remote` in config); `--local` forces this machine.

Drop a file in the incoming directory, then `paddock pull`.

## paths

| | XDG | `.paddock` / `$PADDOCK_DIR` |
|---|---|---|
| config | `~/.config/paddock/config.toml` | `$root/config.toml` |
| store | `~/.local/share/paddock/paddock.db` | `$root/paddock.db` |
| incoming | `~/.local/share/paddock/incoming` | `$root/incoming` |

Resolution: `PADDOCK_DIR`, else walk up from cwd for a `.paddock/` directory, else XDG. `init --here` creates `./.paddock`. Several `[[source]]` blocks share one store. Existing configs are not rewritten by `init`.

## config

```toml
keep = ["todo", "later"]        # labels that never auto-forget
# forget_after = "30d"          # host default for untimed items
# remote = "box"                # ssh host for --remote

[[inbox]]
name = "all"

[[inbox.classifier]]
id = "flag-todo"
kind = "regex"
pattern = "(?i)todo"
label = "todo"

[[inbox.inbox]]
name = "todo"
labels = ["todo"]

[[inbox.inbox]]
name = "cal"
timed = true                    # only items with start; listed in start order

[[inbox.inbox]]
name = "recent"
newer_than = "7d"               # by start, else created_at

[[source]]
id = "incoming"
kind = "fs"
path = "~/.local/share/paddock/incoming"

# [[source]]
# id = "feed"
# kind = "rss"
# url = "https://example.com/feed.xml"

# [[source]]
# id = "mail"
# kind = "exec"
# cmd = "my-mail-source"        # runs `{cmd} {args...} pull` and `{cmd} {args...} send`
# args = []
# forget_after = "14d"

# [[inbox.classifier]]
# id = "by-script"
# kind = "script"               # CEL over `item`: a label, or true with `label =`
# script = 'item.title.contains("invoice") ? "money" : ""'

# [[inbox.classifier]]
# id = "by-llm"
# kind = "llm"                  # Ollama /api/chat or OpenAI-compatible /chat/completions
# model = "llama3.2"
# labels = ["later", "todo"]    # allow-list; the model picks one or NONE
# # prompt = "prefer later unless it is actionable"
```

An item matches an inbox when `(sources empty OR item.source in sources)` and `(labels empty OR item has ALL listed labels)` and (`timed` unset OR the item has `start`) and the age bounds hold, and it matches every ancestor. `keep` labels survive stale cleanup. Lists are queried in SQL, not loaded whole.

A script sees `item.title`, `body`, `source`, `href`, `start`, `end`, `thread`, `read`, `labels`, `parts` (kinds), `from`, `to`. Absent strings are `""`. CEL cannot loop or do IO.

LLM classifiers run on `pull`, once per item (the result is cached). Env: `PADDOCK_LLM_URL`, `PADDOCK_LLM_MODEL`, `PADDOCK_LLM_KEY` or `OPENAI_API_KEY`. Do not put keys in the config.

## exec sources

`kind = "exec"` is the plugin interface. The host runs:

- `{cmd} {args...} pull` — stdout is a JSON array (or NDJSON) of items: `foreign_id` (required), `title`, `body`, `href`, `start`, `end`, `thread`, `from`, `to[]`, `in_reply_to`, `forward_of`, `cite_excerpt`, `cite_actor`, `parts[]`, `read`. Actors are `{id, name?, kind?}`; parts are `{kind, mime, text?, path?}`.
- `{cmd} {args...} send` — stdin is a JSON draft `{title, body, thread?, reply_to_foreign?, to[], parts[]}`; stdout is `{foreign_id, start?, end?}`. Exit 2 (or print `source cannot send`) if the source is read-only.

Admit upserts on `(source_id, foreign_id)`. Re-admit refreshes what the source sent and keeps read state and labels. Cites arrive as foreign ids and resolve on admit; a late parent still stitches.

## license

MIT
