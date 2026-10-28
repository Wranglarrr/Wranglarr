# Live status and API keys

## Live status

The arrs and Prowlarr tell wranglarr when something changes, so its pages show
it straight away. Without this, a fixed problem (an indexer back in Prowlarr, a
health warning cleared in Radarr) shows when the overview's five-minute cache
runs out, or when you press refresh.

wranglarr keeps a webhook called `wranglarr · live status` in each arr's
**Settings → Connect**. It puts it in at startup, once each app answers, and
retries the ones that don't for 15 minutes. The arr calls it on a health issue
and when one clears, a grab, an import, an upgrade, an app update and the
other events it offers. wranglarr then refreshes that app's suite and the open
pages (the Dashboard, the suite pages) redraw it.

Each app signs its calls with its own token (HTTP basic auth: the app's name and
a token wranglarr derives from a secret in its state database). A call without
the right token gets `401`. The webhook path, `POST /api/hooks/<app>`, is the
one path that doesn't need a sign-in, since the arr has no session.

```toml
[events]
enabled = true                       # false takes the webhooks out again
# url = "http://192.168.1.10:8787"   # where the apps reach wranglarr; default http://wranglarr:<port>
```

The arr tests the address before it saves the webhook. If the apps can't reach
wranglarr by its service name (wranglarr on another host, or the arrs outside the
stack's network), set `url`.

**System → Live status** shows, per app, whether the webhook is in place, the
error if it isn't, and the last event since wranglarr started. **Put the
webhooks in again** redoes them now.

## API keys

**Apps → API keys** lists every enabled app's API key, read from the app's own
config file, with its address in the stack and **Show** and **Copy** buttons. It
is there for setting up other apps (a dashboard widget, an outside tool). Only
someone signed in on wranglarr's pages sees it: an API key for wranglarr (a
script, a widget) can't read the apps' keys.

wranglarr has a row too, with its address in the stack. It keeps only a hash of
its own keys, so the row lists them by name, and **New key** makes one (with
your password) and shows it once to copy. Revoke them on your **Account** page.

### A key changed in an app

wranglarr reads each key from the app's config every time, so it follows a new
key by itself. The other apps keep a copy, though: Prowlarr, Bazarr and Seerr
have the arrs' keys, the arrs have SABnzbd's, and the indexers Prowlarr puts in
the arrs carry Prowlarr's.

Once a minute, and whenever an app sends a live status event, wranglarr
compares each key with the one it saw last (it keeps a fingerprint, not the
key). When a key changed, it reconnects every app connected to that one, as
**Connect apps** on the System page does, and Prowlarr pushes its indexers to
the arrs again. If an app had to be reconnected, the
[notification](Notifications.md) *An app's API key changed and wranglarr
reconnected the apps using it* says which (info; a warning if one couldn't be,
with the run to look at). A key change that runs into another system run (an
update) waits for the next minute. A repair stays pending, across restarts,
until it succeeds. After a failure it's tried again in a minute, then 2, 4… up
to once an hour, and the warning is sent once for that key, not on every try.
A newer key change starts over. A repair of an older key can't clear a newer
key change.

## API

| Method | Path | Answer |
| --- | --- | --- |
| POST | `/api/hooks/{app}` | an arr's event: `{"ok": true}`; `401` unknown app or wrong token (no sign-in needed) |
| GET | `/api/events` | `{"enabled", "url", "apps": [{"name", "kind", "action", "error", "wired_at", "last_event"}]}` |
| POST | `/api/events/wire` | puts the webhooks in again; the same, plus `"results"` per app |
| GET | `/api/events/stream` | Server-Sent Events for the pages: `{"type": "hello"}` first, then `{"type": "overview", "suite", "apps", "event"}` and `{"type": "keys", "app"}`; `{"type": "resync"}` if the page fell behind |
| GET | `/api/keys` | `{"apps": [{"name", "kind", "url", "source", "key" or "error"}], "wranglarr": {"url", "auth", "keys": [{"id", "name", "created", "last_used"}]}}`; signed-in pages only (`403` with an API key); never cached |
