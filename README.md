# paddock

An inbox host. TUI and web.

## nouns

- **item** — one thing that arrived, stripped of its source's shape
- **source** — a plugin that admits items
- **label** — a mark a classifier put on an item
- **inbox** — a named question over the pile (labels + sources + sort), not an account and not a folder

Inboxes nest. A child is a tighter question over its parent's matched items. Classifiers are a plugin role owned by an inbox; they run when an item enters that inbox, then children re-evaluate.

## install

```
cargo install --path .
```

## commands

```
paddock init     # config + data dir + incoming (idempotent)
paddock          # TUI; init if needed
paddock pull     # pull sources, classify new items
paddock serve    # http://127.0.0.1:4736
```

Drop a file in the incoming directory, then `paddock pull` (or just wait — the TUI and `serve` watch that directory).

## paths

| | default |
|---|---|
| config | `$XDG_CONFIG_HOME/paddock/config.toml` or `~/.config/paddock/config.toml` |
| data | `$XDG_DATA_HOME/paddock` or `~/.local/share/paddock` |
| store | `$data/paddock.db` |
| incoming | `$data/incoming` |

## config

```toml
[[inbox]]
name = "all"

[[inbox.classifier]]
id = "flag-rfc"
kind = "regex"
pattern = "(?i)rfc"
label = "rfc"

[[inbox.inbox]]
name = "later"
labels = ["later"]

[[source]]
id = "incoming"
kind = "fs"
path = "~/.local/share/paddock/incoming"

# [[source]]
# id = "feed"
# kind = "rss"
# url = "https://example.com/feed.xml"
```

An item matches an inbox when `(sources empty OR item.source in sources)` and `(labels empty OR item has ALL listed labels)`, and it matches every ancestor.

Classifier `kind`: `regex` (title or body), `script` and `llm` (stubs).

## tui

```
j/k ↑↓   move
tab      pane
enter    read
space    toggle read
l        toggle label
r        pull
?        help
q        quit
```

## license

MIT
