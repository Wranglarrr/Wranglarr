# The wranglarr calls

wranglarr talks to the arrs for you; you never need an arr's own API key.
You need wranglarr's address (e.g. `http://192.168.1.10:8787`) and an **API
key** the person creates under Account → API keys. Send it as the `X-Api-Key` header.

```sh
WR=http://192.168.1.10:8787; KEY=wr_...
api() { curl -fsS -H "X-Api-Key: $KEY" -H 'Content-Type: application/json' "$@"; }
```

Only `apply`, `remove` and `undo` change anything. Everything else reads.

## Read

```sh
api "$WR/api/configs"                       # apps that take configs, the examples, the import history
api "$WR/api/configs/inventory/sonarr"      # what sonarr has, part by part (names + one-line summaries)
api "$WR/api/configs/examples/anime-english-dub"
```

The inventory lists, per part, `{"name", "detail", …}`. Quality sizes also
carry `min`, `preferred` and `max`, and quality profiles carry the custom
formats they score. A part the app doesn't have is `null`; an empty list
means it has none yet. The inventory also has the app's `kind` and the
browser `url` of the app.

For the full definitions (every condition, regex and score), export the items:

```sh
api -X POST "$WR/api/configs/export" -d '{
  "name": "current setup",
  "apps": {"sonarr": {"qualityProfiles": ["HD-1080p"], "customFormats": ["x265"], "withFormats": true}}
}'
```

The answer is `{"config": <a wranglarr-config/1 object>, "notes": [...]}`:
the same format you write, so you can edit it and check it.

## Test a release name

```sh
api -X POST "$WR/api/configs/try" -d '{"app": "sonarr", "title": "Show.S01E01.1080p.WEB-DL.DUAL.x265-GRP"}'
```

The answer has the parsed `quality` and `languages`, the custom `formats` that
match, and per profile its `score`, `min` and whether it's `allowed`. Use it to
explain a past grab and to test regex before handing a config over.

## Check a config (no changes)

```sh
api -X POST "$WR/api/configs/check" -d "$(jq -n --rawfile t config.json '{text: $t}')"
```

Optional keys:

- `targets`: `{"sonarr": ["sonarr"]}`, which instances each part goes to.
- `extras`: `{"sonarr/customFormats/10bit": false}`.
- `resolve`: `{"sonarr|qualityProfiles|HD-1080p": "change"|"keep"|"prefix"}`, for items the app already has.
- `prefix`.

The answer has a `plans` list, one per target app. Each plan has `rows` (one
per item, with its `action`: new, change, same or problem), the details and
warnings, and counts of `changes`, `errors` and `warnings`. Fix every error
before suggesting the config. Explain each warning (built-in profile, app-wide
setting, used by N series) to the person.

Each plan also has `protected`: the built-in items, app-wide parts and default
delay profile the config changes, and the items another installed config set
(`overrides` names it). When the top-level `ack` is true, tell the person what's
listed there. Apply needs `"ack": true` for those, and only once they've agreed.

## Apply, see what's installed, remove (changes the apps)

Only when the person asks for it, after they've seen the check:

```sh
api -X POST "$WR/api/configs/apply" -d "$(jq -n --rawfile t config.json '{text: $t, ack: true}')"   # same body as check
api "$WR/api/configs/installed"                                   # each app's timeline, newest first
api "$WR/api/configs/installed/<app>/<id>"                        # what removing an install would do (changes nothing)
api -X POST "$WR/api/configs/installed/<app>/<id>/remove" -d '{"throughout": false}'
```

Each app keeps a timeline of keyframes. The values from before wranglarr changed
them come first, then every install, then each change wranglarr noticed made in the
app itself (`External manual change`). Read it to answer "what did that
config change" or "what did I change by hand since".

Removing an install replays the timeline without it. The preview's `mode`:

- `none`: nothing in the app changes;
- `confirm`: only its own changes go back;
- `choose`: later changes touch the same settings or use what it made, so the
  person picks. `throughout: false` keeps the later changes and what they use.
  `true` puts its settings back to how they were before it, over later changes,
  manual ones included.

Show the person the preview's `effects`, `kept` and `overrides` for the way they
pick before removing.

Neither install nor removal checks yet which films, series or artists use
what they change. Before removing an install that deletes a quality or
metadata profile, ask the person to check that nothing in their library is
still set to it. The app may refuse to delete a profile in use (the removal
then keeps it and returns a note).

`POST /api/configs/history/<id>/undo` is the older undo. It puts back the
copies saved at import, so edits made since to those items are lost. Prefer remove.

Pointing films or series at a profile is not part of a config; tell the person
to use the arr's mass editor.
