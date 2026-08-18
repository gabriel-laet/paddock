# paddock

An inbox host. TUI and web.

## nouns

- **item** — one thing that arrived, stripped of its source's shape
- **source** — a plugin that admits items
- **label** — a mark a classifier put on an item
- **inbox** — a named question over the pile (labels + sources + sort), not an account and not a folder

Inboxes nest. A child is a tighter question over its parent's matched items. Classifiers are a plugin role owned by an inbox; they run when an item enters that inbox, then children re-evaluate.

A label change re-runs classify (classify-on-enter) so a newly matching child can fire. `all/todo` is the todo list — same machinery, no extra feature.

## install

```
cargo install --path .
```

## commands

```
paddock init     # config + data dir + incoming + themes (idempotent)
paddock          # TUI; init if needed
paddock pull     # pull sources, classify new items
paddock serve    # http://127.0.0.1:4736
```

Drop a file in the incoming directory, then `paddock pull` (or just wait — the TUI and `serve` watch that directory).

## paths

| | default |
|---|---|
| config | `$XDG_CONFIG_HOME/paddock/config.toml` or `~/.config/paddock/config.toml` |
| themes | `$config_dir/themes/<name>.toml` |
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

[[inbox.classifier]]
id = "flag-todo"
kind = "regex"
pattern = "(?i)todo"
label = "todo"

[[inbox.inbox]]
name = "later"
labels = ["later"]

[[inbox.inbox]]
name = "todo"
labels = ["todo"]

[[source]]
id = "incoming"
kind = "fs"
path = "~/.local/share/paddock/incoming"

# [[source]]
# id = "feed"
# kind = "rss"
# url = "https://example.com/feed.xml"

# theme = "phosphor"
```

An item matches an inbox when `(sources empty OR item.source in sources)` and `(labels empty OR item has ALL listed labels)`, and it matches every ancestor.

Classifier `kind`: `regex` (title or body), `script` and `llm` (stubs).

Existing configs are not rewritten by `init`.

## tui

```
j/k ↑↓     move
gg / G     top / bottom
C-d / C-u  half page
C-f / C-b  page
h / l      tree / items
tab        swap pane
gt / gT    next / prev inbox
enter      read
space      toggle read
u          unread
dd         eat (mark read)
/ n N      search (title+body)
L          label (toggle + classify)
:          command
?          help
esc        back
q          quit (list)
```

Read: `j/k` next item, `esc`/`h` back, `:` still works, `q` quits.

### colon

```
:q :quit     quit
:pull        pull
:help        keys
:theme NAME  load $config_dir/themes/NAME.toml
:themes      list themes
:why         why this item is in this inbox
:again       reclassify
:eat         mark read
:bury        label later + classify → all/later
:todo        label todo + classify → all/todo
:yank        write title to $data/yank
:open        show href; xdg-open if possible
:new TITLE   write $incoming/TITLE.md and admit
:which       inbox path + counts
:db          store path
:only        unread-only filter
:spill       write current inbox to $data/spill.md
```

Unknown: `not an editor command: foo`.

Web uses the same key table and the same `run_verb` (endpoint `/x/{verb}`).

## theme

TOML. All slots optional; missing = carbon.

```toml
# paddock-theme 1
name = "carbon"

[colors]
bg = "#0a0a0a"
fg = "#d2d2c8"
accent = "#d8c070"
dim = "#666660"
unread = "#f2f2ea"
border = "#222220"
select = "#282828"
```

Load: `theme = "name"` in config.toml (or `:theme NAME`) → `$config_dir/themes/<name>.toml` → built-in carbon.

`paddock init` copies bundled `carbon` and `phosphor` into `$config_dir/themes/` if missing.

TUI maps slots to colors. Web sets `:root { --bg --fg --accent --dim --unread --border --select }`.

## license

MIT
