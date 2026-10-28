# Sonarr (config key `sonarr`)

TV series → seasons → episodes. Checked against Sonarr 4.0.20. Everything here
goes under `"apps": {"sonarr": {…}}`. For how the parts work in general
(ranking, how conditions combine), see `arr-settings.md`.

Things a config can't set in Sonarr, which the person does by hand after
importing:
- which profile a series uses;
- its **Series Type**;
- monitoring, root folders, indexers and download clients.

**Series Type** matters for a config. Standard, Daily and Anime series are
parsed differently (anime uses absolute episode numbers) and are named with
different formats. Anime configs usually also need an auto tagging rule on
`SeriesTypeSpecification`, so tagged release and delay profiles reach them.

## Qualities

Lowest to highest, as Sonarr ranks them by default (`[…]` is a shipped group):

Unknown, SDTV, [WEB 480p: WEBRip-480p, WEBDL-480p], DVD, Bluray-480p,
Bluray-576p, HDTV-720p, HDTV-1080p, Raw-HD, [WEB 720p: WEBRip-720p,
WEBDL-720p], Bluray-720p, [WEB 1080p: WEBRip-1080p, WEBDL-1080p],
Bluray-1080p, Bluray-1080p Remux, HDTV-2160p, [WEB 2160p: WEBRip-2160p,
WEBDL-2160p], Bluray-2160p, Bluray-2160p Remux

Sonarr's remuxes are `Bluray-1080p Remux` / `Bluray-2160p Remux` (Radarr's are `Remux-1080p`).
`Raw-HD` is an uncompressed HDTV capture (very large).

## Quality profiles

Built-in names: `Any`, `SD`, `HD-720p`, `HD-1080p`, `Ultra-HD`, `HD - 720p/1080p`.

| Key | Effect in Sonarr |
| --- | --- |
| `qualities` | allowed qualities, best first; groups make members equal |
| `cutoff` | quality/group where quality upgrades stop |
| `upgradeAllowed` | off = the first acceptable file stays |
| `minFormatScore` | releases scoring less are refused |
| `cutoffFormatScore` | score upgrades stop here (once the cutoff quality is reached) |
| `minUpgradeFormatScore` | an upgrade must beat the current score by this much |
| `formatScores` | custom format name → score |

Sonarr 4 has **no language setting on profiles** (language profiles were
removed in v4). To want or refuse a language, score `LanguageSpecification`
custom formats (e.g. −10000 for `exceptLanguage: true` on English, with
`minFormatScore` 0).

## Quality sizes

`min` / `preferred` / `max` in **MB per minute** of the episode's runtime. A 45-minute episode at
`max` 100 may be at most 4.5 GB. Sonarr ships with high
minimums for 2160p (35 MB/min). That turns away small x265 4K releases, so a size-focused
config may lower them. A **season pack** is judged as one release against the
whole season's runtime.

## Custom format conditions

| `implementation` | Tests | `fields` |
| --- | --- | --- |
| `ReleaseTitleSpecification` | regex on the release name | `{"value": "<regex>"}` |
| `ReleaseGroupSpecification` | regex on the parsed group | `{"value": "^(GRP)$"}` |
| `LanguageSpecification` | parsed language of the release | `value`: `Original` (the series' own language), `Unknown`, `English`, `Japanese`, … (48 languages); `exceptLanguage: true` = any language *but* this one |
| `SourceSpecification` | parsed source | `Unknown`, `Television` (HDTV), `TelevisionRaw` (Raw-HD), `Web` (WEB-DL), `WebRip`, `DVD`, `Bluray`, `BlurayRaw` (remux) |
| `ResolutionSpecification` | parsed resolution | `R360p`, `R480p`, `R540p`, `R576p`, `R720p`, `R1080p`, `R2160p` (`Unknown`) |
| `SizeSpecification` | size in GB: more than `min`, at most `max` | `{"min": 0.5, "max": 1000}`. A season pack counts as one release |
| `IndexerFlagSpecification` | an indexer flag | `Freeleech`, `Halfleech`, `DoubleUpload`, `Internal`, `Scene`, `Freeleech75`, `Freeleech25`, `Nuked` |
| `ReleaseTypeSpecification` | episode count of the release | `Single Episode`, `Multi-Episode`, `Season Pack` (`Unknown`) |

Common Sonarr jobs:
- **Prefer season packs:** score `Season Pack` positive.
- **Avoid multi-episode files:** score `Multi-Episode` negative.
- **Anime audio language:** use `LanguageSpecification` plus title regex, since
  anime release names rarely carry clean language tags.
- **Avoid HDTV:** score `Television` negative.

## Release profiles

`required` (one term must appear), `ignored` (no term may appear), `tags`,
`enabled`. Terms are plain text or `/regex/`. Sonarr 4 dropped "preferred
words", so ranking is done with custom formats.

## Delay profiles

`preferredProtocol`, `usenetDelay`, `torrentDelay` (minutes), `enableUsenet`,
`enableTorrent`, `bypassIfHighestQuality`, `bypassIfAboveCustomFormatScore` +
`minimumCustomFormatScore`, `tags`. The untagged one is the default for every series.

## Auto tagging conditions

They test the **series**, not a release:

| `implementation` | `fields` |
| --- | --- |
| `SeriesTypeSpecification` | `value`: `Standard`, `Daily`, `Anime` |
| `GenreSpecification` | `value`: list of genre names (`["Animation", "Documentary"]`) |
| `OriginalLanguageSpecification` | `value`: a language (`Japanese`) |
| `QualityProfileSpecification` | `value`: a profile name |
| `StatusSpecification` | `status`: `Continuing`, `Ended`, `Upcoming`, `Deleted` |
| `MonitoredSpecification` | none: the series is monitored |
| `TagSpecification` | `value`: a tag name |
| `YearSpecification` | `min`, `max`: first-aired year |
| `RootFolderSpecification` | not allowed in a config (a path) |

`negate` and `required` work as in custom formats.

## File naming (`naming`, app-wide)

| Field | Values |
| --- | --- |
| `renameEpisodes` | `true` renames imported files with the formats below; `false` keeps release names |
| `replaceIllegalCharacters` | `true` replaces `\ / : * ? " < > |`, `false` removes them |
| `colonReplacementFormat` | `0` delete, `1` dash, `2` space-dash, `3` space-dash-space, `4` smart, `5` custom |
| `customColonReplacementFormat` | the text for `5` |
| `multiEpisodeStyle` | `0` extend S01E01-02-03, `1` duplicate S01E01.S01E02, `2` repeat S01E01E02E03, `3` scene S01E01-E02-E03, `4` range S01E01-03, `5` prefixed range S01E01-E03 |
| `standardEpisodeFormat`, `dailyEpisodeFormat`, `animeEpisodeFormat` | file name per series type |
| `seriesFolderFormat`, `seasonFolderFormat`, `specialsFolderFormat` | folder names |

Tokens:
- **Series:** `{Series Title}`, `{Series TitleYear}`, `{Series CleanTitle}`,
  `{Series TitleThe}`, `{Series TitleWithoutYear}`, `{Series Year}`,
  `{Series TitleFirstCharacter}`.
- **Ids:** `{TvdbId}`, `{TmdbId}`, `{ImdbId}`, `{TvMazeId}`.
- **Numbers and dates:** `{season:00}`, `{episode:00}`, `{absolute:000}`,
  `{Air-Date}`, `{Air Date}`.
- **Episode:** `{Episode Title}`, `{Episode CleanTitle}`.
- **Quality:** `{Quality Full}`, `{Quality Title}`.
- **Media info:** `{MediaInfo Simple}`, `{MediaInfo Full}`,
  `{MediaInfo VideoCodec}`, `{MediaInfo VideoBitDepth}`,
  `{MediaInfo VideoDynamicRange}`, `{MediaInfo VideoDynamicRangeType}`,
  `{MediaInfo AudioCodec}`, `{MediaInfo AudioChannels}`,
  `{MediaInfo AudioLanguages}`, `{MediaInfo SubtitleLanguages}`.
- **Release:** `{Release Group}`, `{Custom Formats}`, `{Custom Format:Name}`,
  `{Release Hash}`, `{Original Title}`, `{Original Filename}`.

Use `.` instead of spaces inside a token (`{Series.CleanTitle}`) for dotted names. Text
inside the braces next to the token, as in `{-Release Group}`, is only written when the token has a value.

## Media management (`mediaManagement`, app-wide, path-free options only)

| Field | Values and effect |
| --- | --- |
| `downloadPropersAndRepacks` | `preferAndUpgrade`: always prefer a proper/repack and upgrade to it. `doNotUpgrade`: prefer them, but don't replace a file just for one. `doNotPrefer`: treat them like any release, so custom formats decide |
| `episodeTitleRequired` | `always`, `bulkSeasonReleases`, `never`: hold imports until the episode title is known (when the naming format uses it) |
| `autoUnmonitorPreviouslyDownloadedEpisodes` | unmonitor an episode whose file was deleted outside Sonarr |
| `createEmptySeriesFolders` | create the series folder when a series is added |
| `deleteEmptyFolders` | delete empty series and season folders during a disk scan |
| `fileDate` | file modified time: `none`, `localAirDate`, `utcAirDate` |
| `rescanAfterRefresh` | `always`, `afterManual`, `never` |
| `importExtraFiles` + `extraFileExtensions` | import matching extra files (`"srt,nfo"`) next to the video |
| `enableMediaInfo` | read codec details from files (feeds the MediaInfo tokens) |
