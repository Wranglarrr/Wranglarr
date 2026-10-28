# What the arr settings do (all arrs)

The rules every arr shares. Each app's options (qualities, condition types and
their values, auto tagging conditions, naming tokens and fields, media
management options) are in its own file: `radarr.md`, `sonarr.md`,
`lidarr.md`, `whisparr.md`. Those were checked against Radarr 6.4, Sonarr
4.0.20 and Lidarr 3.1. Values can change between versions: the person's
inventory lists their qualities, and `POST /api/configs/check` names any
condition type or value their app doesn't have.

## How a release gets picked

For each release a search or RSS turns up, the arr:

1. **Parses the name**: title, year/season/episode, quality (source +
   resolution, e.g. `WEBDL-1080p`), release group, languages, edition…
2. **Rejects it** when any of these hold:
   - its quality isn't allowed in the item's quality profile;
   - its size per minute is outside that quality's quality sizes;
   - it misses a release profile's required terms or has an ignored term;
   - its custom format score is under the profile's `minFormatScore`;
   - plus things a config can't touch (blocklist, indexer rules, already have it).
3. **Ranks what's left**:
   - first by **quality, in the profile's order**. Qualities in one group count as equal;
   - then by **custom format score** (the sum of the scores of every format that matches);
   - then smaller tie-breaks (preferred protocol, indexer priority, seeders, age, size).
4. **Waits** if a delay profile says to (see below), then grabs the best.

Upgrades follow the same ranking. A file on disk is replaced by a release
that ranks higher, until the file has the profile's **cutoff** quality *and*
at least `cutoffFormatScore`. A score-only upgrade must beat the current
file's score by at least `minUpgradeFormatScore`. With `upgradeAllowed:
false` the first acceptable grab is final.

Because quality comes first, a good custom format can't lift a lower quality
over a higher one. To let scores decide between qualities, put them in one
**group** in the profile. The x265 example does this: one group from SD to
1080p, then resolution custom formats scored in tiers.

## Custom formats

A custom format is a named test made of **conditions** (`specifications`).
A release matches the format when:

- every condition marked `required` matches; and
- for each condition **type** (`implementation`) used in the format, at least
  one condition of that type matches.

Same type = OR, different types = AND, `required` = must match. Example: two
`ReleaseTitleSpecification`s and one `SourceSpecification` match "(title A or
title B) and source". `negate: true` flips a condition: it matches when its test fails.

Typical patterns:

- **Boost or penalise something.** One format per thing (`x265`, `10bit`,
  `Remux`), each with a positive or negative score in the profile.
- **Tiers.** Formats per resolution or source with spaced scores (2000/1500/1000) so
  a tier always beats any mix of smaller bonuses below it.
- **Size steps.** `SizeSpecification` formats (`>3GB`, `>4GB`…) at −5 each add up, so
  smaller wins within a tier.
- **Hard no.** A large negative score (−10000) together with a profile
  `minFormatScore` of 0 or more keeps those releases out entirely.
- **Need one of these.** Give the format a score and set the profile's
  `minFormatScore` to that score, so only releases that match get grabbed.

`includeCustomFormatWhenRenaming: true` lists the format in the `{Custom
Formats}` naming token.

### Condition types

Every arr has `ReleaseTitleSpecification` (regex on the release name),
`ReleaseGroupSpecification` (regex on the group), `SizeSpecification` (GB:
more than `min`, at most `max`) and `IndexerFlagSpecification`. Radarr and
Sonarr add source, resolution and language. Radarr adds edition, quality
modifier and year, and Sonarr adds release type. The per-app files list the
fields and every allowed value. Choice values can be given by the label the
arr shows or by its number.

### Regex

- .NET flavour, matched case-insensitively against the whole name.
- Separators in release names are dots, spaces, dashes and underscores. Use `[ ._-]`
  between words (`english[ ._-]?dub`).
- `\b` treats `_` as a word character and digits as part of words. For whole
  words use lookarounds: `(?<![a-z0-9])(dual|multi)(?![a-z0-9])`.
- Short words collide with titles (`Dual!`, `Multiverse`). Anchor on where the
  tag sits, e.g. after the resolution: `\d{3,4}p.*(?<![a-z0-9])multi(?![a-z0-9])`.
- In JSON, every backslash is doubled: `"\\bHEVC\\b"`.

## Quality profiles

| Field | Meaning |
| --- | --- |
| `qualities` | the allowed qualities, **best first**. A group `{"name", "qualities": [...]}` makes its members equal. Anything not listed is refused |
| `cutoff` | the quality (or group) where quality upgrades stop |
| `upgradeAllowed` | whether files get upgraded at all |
| `formatScores` | custom format name → score |
| `minFormatScore` | releases scoring less are refused |
| `cutoffFormatScore` | score at which score upgrades stop (with the cutoff quality reached) |
| `minUpgradeFormatScore` | how much higher a release must score to replace the file |
| `language` | Radarr only: the language the profile wants (`Original`, `English`, `Any`…). Sonarr 4 has no profile language; use `LanguageSpecification` formats |

A profile only applies to the films or series set to it. A config adds or
changes the profile, and the person moves items to it with the arr's mass editor.

Built-in profile names (change the original if reused): Radarr/Sonarr `Any`,
`SD`, `HD-720p`, `HD-1080p`, `Ultra-HD`, `HD - 720p/1080p`; Lidarr `Any`,
`Lossless`, `Standard`.

## Quality sizes (app-wide)

Per quality: `min`, `preferred` and `max` (`null` = no limit). A release
whose size is outside min–max for its length is refused, and of the rest the
arr leans toward `preferred`. The unit differs:

- Radarr, Sonarr, Whisparr: **MB per minute** of runtime. 10 MB/min for an
  hour is about 600 MB, and 100 MB/min about 6 GB.
- Lidarr: an average **bitrate in kbit/s** (320 for MP3-320).

These apply to every profile in the app. Change them only when the job needs
it, and keep minimums low unless the person wants size gates.

## Release profiles

`required`: at least one term must be in the name. `ignored`: none may be.
Terms are plain text, or a regex between slashes (`/\bHDCAM\b/`).
`tags` limits the profile to films/series with one of those tags (no tags = everything).
`enabled` switches it off without deleting it. For ranking rather than
refusing, use custom formats instead.

## Delay profiles

They hold a grab until a release is old enough, so a better one can turn up:
`usenetDelay` / `torrentDelay` in minutes, `preferredProtocol`,
`enableUsenet` / `enableTorrent`. Bypasses: `bypassIfHighestQuality`, and
`bypassIfAboveCustomFormatScore` with `minimumCustomFormatScore`. A profile with tags
applies to items with those tags. The one without tags is the app's default for everything else, and is app-wide.
Lidarr gives its protocols as `items`.

## Tags

Tags connect things: a release profile, delay profile, indexer, download
client or notification with a tag only applies to films/series with that tag.
Names are lower-case letters, digits and dashes. A config creates any tag it names.

## Auto tagging

Rules that tag library items automatically: `name`, the `tags` to add,
`removeTagsAutomatically`, and `specifications` in the custom format shape,
with conditions about the library item (series, film or artist) rather than a
release: its genre, status, original language, quality profile, year, tag,
whether it's monitored, and per app more (Sonarr's series type, Radarr's
keywords, studio and runtime, Lidarr's metadata profile). The per-app files
list them with their fields. Quality profiles, tags and choices are given by name. Root-folder conditions are refused
(a path). Adding a rule tags matching items in the library on the next refresh.

## File naming and media management (app-wide)

`naming` takes the app's naming fields by their API names, with the app's
tokens. `mediaManagement` takes only options that name no path. Both are one
set per app, so a config that changes them changes every import from then on.
The fields, values and tokens differ per app (see the per-app files).

`downloadPropersAndRepacks: "doNotPrefer"` lets custom formats decide on
propers and repacks instead of always preferring them. It's the usual setting
when a config scores repacks itself.
