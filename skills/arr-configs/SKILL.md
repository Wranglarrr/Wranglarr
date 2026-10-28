---
name: arr-configs
description: Read, explain and write *arr settings (Radarr, Sonarr, Lidarr, Whisparr) through wranglarr's community configs - the wranglarr-config/1 JSON format, wranglarr's config API, and each arr's own options (custom formats and their conditions/regex, quality profiles and scores, quality sizes, metadata profiles, release and delay profiles, tags, auto tagging, file naming, media management) and what each does in that app. Use when someone wants to understand their current arr setup, figure out why a release was or wasn't grabbed, or get a profile, custom format, tag or regex set that does a job ("x265 preferred", "English dub anime only", "no cams", "smaller files"), delivered as a wranglarr-config/1 JSON they can check and import.
---

# arr configs with wranglarr

wranglarr imports and exports arr settings as one JSON file per job, the
**wranglarr-config/1** format. Your output for a "make me a profile" request is
such a file. The person pastes it into wranglarr (Community configs → Import),
reads the preview of what would change, and applies it. Every install can be removed again.

## How to work

1. **Find out what they have.** With wranglarr's address and an API key
   (`references/api.md`), read each app's inventory and export the items
   that matter. Without access, ask them to export the relevant items from the
   Community configs page and paste the JSON. Don't guess at names: profiles,
   custom formats, tags and qualities are matched by exact name.
2. **Find out what they want, in outcomes.** "Smaller files", "never a cam",
   "English audio for anime", "4K only when it's HDR". Ask which app (and which
   instance, when there are two Radarrs), and whether existing films or series
   should move to the new profile (a config can't do that; they use the arr's
   mass editor).
3. **Design with the arr's real rules.** `references/arr-settings.md` has
   what every arr shares: how conditions combine inside a custom format, how
   quality order and scores rank releases, and what cutoff and upgrade scores
   do. Most mistakes come from assuming a custom format is a simple OR of its
   conditions. Then read the file for the app you're writing for
   (`references/radarr.md`, `sonarr.md`, `lidarr.md`, `whisparr.md`). It has
   that app's quality names, condition types with every allowed value, auto
   tagging conditions, naming fields and tokens, and media management options.
   These differ between apps.
4. **Write the config** (`references/format.md`). Give new items names of their
   own (a common prefix such as `x265-` keeps them together and clear of the
   person's own). Mark the parts people disagree on as optional `extra`s with
   a short note.
5. **Test before handing over.** Run real release names through
   `POST /api/configs/try` and the draft through `POST /api/configs/check`
   (neither changes anything). Fix every *problem* row. Explain each *change*
   row that touches something of theirs.
6. **Hand it over.** Give the JSON and a short summary: what it adds, what
   it changes of theirs, what they still do by hand (switch series to the profile,
   set Series Type to Anime…). Applying changes their apps, so apply only when
   they ask you to, and say first what will change.

## Rules that keep a setup safe

- **Built-in names change the original.** A config naming `HD-1080p` or `Any`
  changes that profile wherever it's imported. Prefer a new name.
- **App-wide parts affect every profile.** Quality sizes, file naming, media
  management and the untagged (default) delay profile are one set per app. Change
  them only when the job needs it, and say so.
- **Scores you leave out stay as they are.** A profile's `formatScores` only
  sets the formats it lists. In a new profile the rest score 0.
- **Paths never go in a config:** root folders, download paths, scripts. The
  importer refuses root-folder auto tagging conditions.
- **Regex is .NET and case-insensitive.** Anchor words with lookarounds
  (`(?<![a-z0-9])dub(?![a-z0-9])`) instead of `\b` when the separator can be `_`
  or a dot next to digits. Test the regex against names that should match *and*
  names that shouldn't.
- **Release names lie.** Say how sure a rule can be, and suggest testing real names.

## Reading a setup back to someone

When asked "why did it grab X" or "what does my profile do":

- **Rank by quality first.** Releases are ranked by quality in the profile's
  order first (a group's qualities count as equal), then by custom format score.
- **Check the gates.** A release under `minFormatScore`, outside the quality
  sizes, or matching a release profile's ignored term is rejected outright.
- **Check the upgrade rule.** Upgrades stop once the file has the cutoff
  quality *and* `cutoffFormatScore`. Each upgrade must beat the current score
  by `minUpgradeFormatScore`.
- **Show your working.** Use `/api/configs/try` with the exact release name.
  It answers with the parsed quality, the matching custom formats and each
  profile's score.

## References

| File | What's in it |
| --- | --- |
| `references/format.md` | the wranglarr-config/1 JSON, key by key, with a full example and a checklist |
| `references/api.md` | wranglarr's API for configs: inventory, export, try, check, apply, the installed timeline and removal (API key, curl) |
| `references/arr-settings.md` | what every arr shares: how a release gets picked, how conditions combine, profile fields, sizes, release and delay profiles, tags, auto tagging |
| `references/radarr.md` | Radarr: qualities, profile language, every condition type and value (edition, quality modifier, year…), auto tagging conditions, naming fields and tokens, media management |
| `references/sonarr.md` | Sonarr: qualities, series types, conditions (release type, language…), auto tagging (series type…), naming (episode styles, colon options) and media management |
| `references/lidarr.md` | Lidarr: lossy/lossless qualities, sizes as bitrate, metadata profiles, its four condition types, delay profile `items`, naming and media management |
| `references/whisparr.md` | Whisparr: which of the above applies (v3 follows Radarr, v2 Sonarr) |

Readarr doesn't take configs.
