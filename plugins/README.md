# paddock exec plugins

Python 3 wrappers for `gog`, `hey`, and `wacli`. The host runs `{cmd} pull` or `{cmd} send`.

Do not add these to default `paddock init`. Point a source at a script:

```toml
[[source]]
id = "gog-mail"
kind = "exec"
cmd = "/path/to/paddock/plugins/gog-mail"

[[source]]
id = "gog-cal"
kind = "exec"
cmd = "/path/to/paddock/plugins/gog-cal"

[[source]]
id = "hey-mail"
kind = "exec"
cmd = "/path/to/paddock/plugins/hey-mail"

[[source]]
id = "hey-cal"
kind = "exec"
cmd = "/path/to/paddock/plugins/hey-cal"

[[source]]
id = "wacli"
kind = "exec"
cmd = "/path/to/paddock/plugins/wacli"
```

Scripts are executable (`#!/usr/bin/env python3`) and import `lib.py` from this directory.

## binaries / env

| plugin | binary | override |
|---|---|---|
| gog-mail, gog-cal | `gog` | `PADDOCK_GOG` |
| hey-mail, hey-cal | `hey` | `PADDOCK_HEY` |
| wacli | `wacli` | `PADDOCK_WACLI` |

- `GOG_ACCOUNT` or `PADDOCK_GOG_ACCOUNT` — one account. If unset, `gog --json --no-input auth list` and every account is pulled.
- `GOG_KEYRING_PASSWORD` is passed through if already set. Plugins never print it.
- `PADDOCK_PLUGIN_FIXTURE` — path to a JSON file. `pull` reads that instead of calling the CLI (tests). Shape may be the raw CLI envelope.

Pull uses `gog --readonly --no-input --json` (plus `--results-only` when the binary accepts it).

## send

- gog-mail / hey-mail / wacli: send when the CLI can.
- gog-cal / hey-cal: exit 2 `source cannot send` (compose has no start/end yet; this `hey` binary has no event create).

## testdata

Fixtures under `testdata/` are fake. Tests must not call live `gog` / `hey` / `wacli`.
