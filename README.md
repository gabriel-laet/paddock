# paddock

An inbox kernel with a CLI. No UI. Meant to be driven by hand, by scripts, and by agents.

The kernel is pure: four nouns, the questions inboxes ask, and the verbs. It reads no clock, no file, no network, and no config format; the host resolves every adapter up front and hands the kernel a config, a store, sources, classifiers, an embedder, a model, and the time. Every verb returns what happened, warnings included.

One repository, three crates and the plugins, with the dependency graph enforcing the lines:

```
crates/kernel/      paddock-kernel: nouns, questions, verbs, ports. Its Cargo.toml is the purity
                    test: no store, no file format, no network, no clock.
crates/protocol/    paddock-protocol: the exec protocol's types and the Message shape.
                    Plugins depend on this and nothing else of paddock's. See PROTOCOL.md.
crates/paddock/     the `paddock` binary: adapters (sqlite store; fs and exec sources;
                    script, exec, http, llm classifiers; models; embedders; host) and the CLI
plugins/rss/        paddock-rss. Formats of the world live here, not in the kernel crate.
```

## nouns

- **item** — one thing that arrived, stripped of its source's shape
- **source** — a program that admits items (and may send); anything that knows a format of the world is a plugin over the exec protocol
- **label** — a mark a classifier or a hand put on an item; it remembers which, and a hand outranks a classifier
- **inbox** — a named question over the pile (sources, labels, `without`, time, words), not an account and not a folder; it may do something to what enters it

Items may have parts (text, file, image, audio, video) and an optional thread. `body` is the preview; the full text is the first text part. Part bytes live in the store, not on disk beside it.

Actors and cites are kernel: an item can have `from` / `to` (a person, a group, or a list) and `cites`. A cite is one shape for a reply, a forward, a quote, a mention, or an attachment: it names an item by its source's id (resolved to ours when that item is here, early or late) or something outside the pile by `href`, with an optional excerpt and actor. A thread is the source's key when it has one, else whatever replies and forwards join.

A label a hand removed stays denied: no classifier, source, or effect puts it back until a hand does. `why` shows who stamped each label and what is denied. Read is the label `read`, so "unread" is `without = ["read"]`, and a hand's unread beats a source's read.

Inboxes nest. A child is a tighter question over its parent's matched items. Classifiers are owned by an inbox; they run when an item enters that inbox, then the inbox's `then` effects run, then children re-evaluate. A label change re-runs classify (classify-on-enter) so a newly matching child can fire. `all/todo` is the todo list: same machinery, no extra feature.

Personas are top-level inboxes with `sources`: `work/` and `personal/` see only their own sources, nest their own questions, and `send --in work` sends from the first of work's sources. An actor may be a `person`, `group`, `list`, or `agent`.

## install

```
cargo install --path crates/paddock              # the `paddock` binary
cargo install --path plugins/rss                 # `paddock-rss`, for `kind = "rss"`
```

## commands

```
paddock init [--here]              # XDG, or ./.paddock
paddock pull                       # pull sources, classify new items, forget stale
paddock inboxes                    # tree with unread/total
paddock ls [INBOX] [--unread] [--text WORDS] [--like TEXT] [--limit N]
paddock show ID                    # one item in full
paddock thread ID                  # the source's thread, else what replies and forwards join
paddock cited ID                   # items that cite ID
paddock part ID > file             # the bytes of a non-text part
paddock label ID [+l|-l]...        # add / remove labels, then reclassify
paddock read ID | unread ID
paddock forget ID                  # delete
paddock classify ID                # re-run classifiers
paddock why ID [INBOX]             # the chain's labels it carries, who stamped each, what a hand denied
paddock send [--title T] [--reply ID] [--to A]... [--source S | --in INBOX] [BODY]   # body from arg or stdin
paddock answer QUESTION [--in INBOX]   # the model answers from the items and cites them
paddock embed                      # embed items that have no vector yet
paddock check CMD [--set k=v]... [--send]   # speak the protocol to a plugin and validate what it says
paddock context                    # dump this host for an agent
```

`--json` on any command prints machine output. `--dir DIR` names the host directory. `--remote[=HOST]` re-runs the same command over ssh (host from the flag, else `remote` in config); `--local` forces this machine.

Drop a file in the incoming directory, then `paddock pull`.

## paths

| | XDG | `.paddock` / `$PADDOCK_DIR` |
|---|---|---|
| config | `~/.config/paddock/config.toml` | `$root/config.toml` |
| store | `~/.local/share/paddock/paddock.db` | `$root/paddock.db` |
| incoming | `~/.local/share/paddock/incoming` | `$root/incoming` |

Resolution: `--dir`, else `PADDOCK_DIR`, else walk up from cwd for a `.paddock/` directory, else XDG (`XDG_CONFIG_HOME`, `XDG_DATA_HOME`). `init --here` creates `./.paddock`. Existing configs are not rewritten by `init`. Those are the only environment variables paddock reads, and they only say where the host is; keys and endpoints live in the config next to what uses them, or come from a command (below).

## config

The host part, then one block per inbox, source, classifier, and (optionally) the embedder, the model, and the store. Several `[[source]]` blocks share one store.

```toml
keep = ["todo", "later"]        # stale cleanup asks "without these"
# forget_after = "30d"          # host default for untimed items
# remote = "box"                # ssh host for --remote

[[inbox]]
name = "all"

[[inbox.classifier]]            # runs on every item entering `all`
id = "flag-todo"
kind = "regex"
pattern = "(?i)todo"
label = "todo"

[[inbox.inbox]]                 # a child: a tighter question over `all`
name = "todo"
labels = ["todo"]

[[inbox.inbox]]
name = "cal"
timed = true                    # only items with start; listed in start order

[[inbox.inbox]]
name = "recent"
newer_than = "7d"               # by start, else created_at

[[inbox.inbox]]
name = "unread"
without = ["read", "later"]     # carries none of these

[[source]]
id = "incoming"
kind = "fs"
path = "~/.local/share/paddock/incoming"
```

### effects

`then` names what an inbox does to an item that enters it: `label:NAME`, `read`, or `send:SOURCE`. What an effect produces (the item a `send:` delivers) is classified like anything else but fires no effects of its own, so an effect cannot chase its own output. An effect runs once per item per inbox, the same way a run-once classifier does; one that fails (a source that is down) is not remembered, so it retries on the next classify or pull and warns each time until it goes through. `send:` stamps `sent`. That is the whole approval flow: an agent drafts into a source, a hand adds `approved`, the inbox sends.

```toml
[[inbox]]
name = "drafts"
sources = ["drafts"]            # where an agent writes on your behalf

[[inbox.inbox]]
name = "approved"
labels = ["approved"]           # a hand adds this
then = ["send:mail", "label:done", "read"]
```

An item matches an inbox when `(sources empty OR item.source in sources)` and `(labels empty OR item has ALL listed labels)` and `(item has NONE of without)` and (`timed` unset OR the item has `start`) and the age bounds hold, and it matches every ancestor. Stale cleanup is the question `without = keep` and then a passed `end` or an old `created_at`, so `keep` is just a `without`. Lists are queried in SQL, not loaded whole.

### sources

Where items come from. Every source has an `id` (which items carry as `source_id`) and a `kind`; `name` is an optional display name and `forget_after` a per-source stale window. Built-in kinds are `fs` and `exec`; any other kind names a plugin, `paddock-<kind>` on `PATH`, and every other key on the block reaches it as settings.

```toml
[[source]]
id = "incoming"
kind = "fs"                     # a directory: every file is an item; a draft becomes a file
path = "~/.local/share/paddock/incoming"

[[source]]
id = "feed"
kind = "rss"                    # a plugin: `paddock-rss` on PATH; cannot send
url = "https://example.com/feed.xml"

[[source]]
id = "mail"
kind = "exec"                   # an explicit program that speaks the exec protocol
cmd = "~/bin/mail-source"
args = ["--account", "work"]
dir = "~/mail"                  # working directory, optional
forget_after = "14d"
```

### classifiers

What stamps labels. A classifier belongs to an inbox (`[[inbox.classifier]]`, or `[[inbox.inbox.classifier]]` deeper) and runs when an item enters it; a top-level `[[classifier]]` runs on everything. The kernel reads `id`, `kind`, `label`, `labels`, and `once`; every other key belongs to the adapter for that kind. Since it runs on entry, an `llm` classifier on `all` sees every item as it lands. A bad spec fails when the config loads, not when an item arrives.

```toml
[[inbox.classifier]]
id = "flag-todo"
kind = "regex"                  # in the kernel: pattern over title or body
pattern = "(?i)todo"
label = "todo"

[[inbox.classifier]]
id = "by-script"
kind = "script"                 # CEL over `item`; yields a label, or true with `label =`
script = 'item.title.contains("invoice") ? "money" : ""'

[[inbox.classifier]]
id = "by-program"
kind = "exec"                   # item as JSON on stdin; label on stdout
cmd = "~/bin/label-it"
args = []
once = true                     # remember the verdict per item

[[inbox.classifier]]
id = "by-service"
kind = "http"                   # POST the item as JSON; the body is the label
url = "http://127.0.0.1:8080/label"
key = "..."                     # bearer token, optional
once = true

[[inbox.classifier]]
id = "by-claude"
kind = "llm"                    # a prompt from the item, one token back; always once
cmd = "claude"                  # over a CLI: prompt on stdin, reply on stdout
args = ["-p"]
labels = ["later", "todo"]      # allow-list; the model picks one or NONE
prompt = "prefer later unless it is actionable"

[[inbox.classifier]]
id = "by-ollama"
kind = "llm"
provider = "ollama"             # or "openai": url, model, key
model = "llama3.2"
label = "urgent"                # with a single label the model answers yes or no
```

A script sees `item.title`, `body`, `source`, `href`, `start`, `end`, `thread`, `read`, `labels`, `parts` (kinds), `from`, `to`, `cites` (`{kind, id, href}`). Absent strings are `""`. CEL cannot loop or do IO.

A label reply is its first token; `NONE` or nothing means no label; a JSON reply may say `{"label": "..."}`. A classifier that fails (a program that exits non-zero, a service that is down) is a warning, not a verdict, and is tried again next time.

### secrets

Any `key` can instead be `key_cmd`: a shell command whose stdout is the secret, so the config file never holds it.

```toml
[store]
key_cmd = "pass show paddock"   # or: key = "..."

[[inbox.classifier]]
id = "by-service"
kind = "http"
url = "https://example.com/label"
key_cmd = "security find-generic-password -s example -w"
```

### store

Where items live. Missing means sqlite, unencrypted. With a `key` (or `key_cmd`) the file is encrypted with SQLCipher, part bytes included, since parts live inside the store. A plaintext store opened with a key fails with "wrong key, or not an encrypted store"; an encrypted one opened without fails with "not a store, or it is encrypted". Set the key on a fresh store. There is no in-place migration yet.

```toml
[store]
kind = "sqlite"
key_cmd = "pass show paddock"
```

### embedder and model

Optional. The embedder turns text into a vector, once per item on admit, and again for a query; `ls --like` and `answer` need it. The model is what `answer` talks to.

```toml
[embedder]
kind = "ollama"                 # or "openai" | "http" | "exec" | "local"
model = "nomic-embed-text"
# url = "http://127.0.0.1:11434"
# key = "..."

# [embedder]
# kind = "local"                # in-process Model2Vec; build with --features local
# model = "~/models/potion-base-8M"   # a folder with model.safetensors, tokenizer.json, config.json

[model]
kind = "exec"                   # or "ollama" | "openai"
cmd = "claude"
args = ["-p"]
# url, model, key for the http kinds
```

`local` needs `cargo install --path . --features local`; it runs a Model2Vec static model on the CPU with no network, and the default build stays small without it. `exec` embedders get the text on stdin and print a JSON array of numbers. `http` embedders are POSTed `{"text": ..., "model": ...}` and may reply with a bare array or an object holding `embedding`, `vector`, or `data[0].embedding`. `exec` models get the system and user prompt on stdin and reply on stdout.

## search

Words and meaning are both terms of a Question, so they compose with the inbox chain.

- `ls all/work --text "invoice ana"` — every word must appear in the title or text; words match by prefix (FTS5 in the store).
- `ls all/work --like "the thing Ana said about money"` — closest by meaning, through the embedder; `--limit` caps it (default 20).
- `answer "what do I owe Ana?" --in all/work` — retrieves by words and by meaning, hands the model the items, and prints the answer with the items it cited. `--json` gives `{text, cites, considered}`.

Models never run inside matching. An inbox is deterministic and `why` stays true.

## plugins

The exec protocol is the plugin interface: `{cmd} {args...} pull|send` with a JSON request on stdin (the source's id and settings, plus the draft for `send`), items or a `{foreign_id}` on stdout. It is written down in [PROTOCOL.md](PROTOCOL.md), typed in the `paddock-protocol` crate, and checked with `paddock check`. Write a plugin in Rust against that crate, or in anything that can read and print JSON.

Admit upserts on `(source_id, foreign_id)`. Re-admit refreshes what the source sent and keeps read state and labels. A cite to an item not here yet resolves when it arrives.

## license

MIT
