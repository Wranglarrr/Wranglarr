# Radarr (config key `radarr`)

Films. Checked against Radarr 6.4. Everything here goes under
`"apps": {"radarr": {…}}`. For how the parts work in general (ranking, how
conditions combine), see `arr-settings.md`.

Things a config can't set in Radarr, which the person does by hand:
- which profile a film uses;
- its minimum availability;
- monitoring, root folders, lists, indexers and download clients.

## Qualities

Lowest to highest, as Radarr ranks them by default (`[…]` is a shipped group):

Unknown, WORKPRINT, CAM, TELESYNC, TELECINE, REGIONAL, DVDSCR, SDTV, DVD,
DVD-R, [WEB 480p: WEBDL-480p, WEBRip-480p], Bluray-480p, Bluray-576p,
HDTV-720p, [WEB 720p: WEBDL-720p, WEBRip-720p], Bluray-720p, HDTV-1080p,
[WEB 1080p: WEBDL-1080p, WEBRip-1080p], Bluray-1080p, Remux-1080p,
HDTV-2160p, [WEB 2160p: WEBDL-2160p, WEBRip-2160p], Bluray-2160p,
Remux-2160p, BR-DISK, Raw-HD

- **Pre-release and cinema sources:** WORKPRINT, CAM, TELESYNC, TELECINE,
  REGIONAL and DVDSCR. Leave them out of a profile to never grab them.
- **BR-DISK** is a full disc image (folder structure, not a video file). Most
  players can't use it, so most profiles leave it out.

## Quality profiles

Built-in names: `Any`, `SD`, `HD-720p`, `HD-1080p`, `Ultra-HD`, `HD - 720p/1080p`.

| Key | Effect in Radarr |
| --- | --- |
| `qualities` | allowed qualities, best first; groups make members equal |
| `cutoff` | quality/group where quality upgrades stop |
| `upgradeAllowed` | off = the first acceptable file stays |
| `minFormatScore` | releases scoring less are refused |
| `cutoffFormatScore` | score upgrades stop here (once the cutoff quality is reached) |
| `minUpgradeFormatScore` | an upgrade must beat the current score by this much |
| `formatScores` | custom format name → score |
| `language` | the language releases must have: `Original` (the film's own), `Any` (no check), or a language name (`English`, `French`…). Releases parsed as another language are refused |

## Quality sizes

`min` / `preferred` / `max` in **MB per minute** of the film's runtime. A
2-hour film at `max` 100 may be at most 12 GB. Unlimited is `null`.

## Custom format conditions

| `implementation` | Tests | `fields` |
| --- | --- | --- |
| `ReleaseTitleSpecification` | regex on the release name | `{"value": "<regex>"}` |
| `ReleaseGroupSpecification` | regex on the parsed group | `{"value": "^(GRP)$"}` |
| `EditionSpecification` | regex on the parsed edition | `{"value": "<regex>"}`, e.g. `"imax"` |
| `LanguageSpecification` | parsed language of the release | `value`: `Any`, `Original`, `Unknown`, `English`, … (60); `exceptLanguage: true` = any language *but* this one |
| `SourceSpecification` | parsed source | `UNKNOWN`, `CAM`, `TELESYNC`, `TELECINE`, `WORKPRINT`, `DVD`, `TV`, `WEBDL`, `WEBRIP`, `BLURAY` |
| `ResolutionSpecification` | parsed resolution | `R360p` … `R2160p` (`Unknown`) |
| `QualityModifierSpecification` | quality modifier | `NONE`, `REGIONAL`, `SCREENER`, `RAWHD`, `BRDISK`, `REMUX` |
| `SizeSpecification` | size in GB: more than `min`, at most `max` | `{"min": 3, "max": 1000}` |
| `YearSpecification` | the film's year | `{"min": 1900, "max": 1999}` |
| `IndexerFlagSpecification` | an indexer flag | `G Freeleech`, `G Halfleech`, `G DoubleUpload`, `G Internal`, `G Scene`, `G Freeleech75`, `G Freeleech25`, `PTP Golden`, `PTP Approved`, `Nuked` |

Common Radarr jobs:
- **Prefer or avoid remuxes:** `QualityModifierSpecification` `REMUX`.
- **Never take disc images:** `BRDISK` at −10000.
- **Favour editions:** score `EditionSpecification` regexes (IMAX, Director's Cut).
- **Older films differently:** `YearSpecification`, e.g. accept smaller files for pre-1990 films.

## Release profiles

`required`, `ignored` (plain text or `/regex/`), `tags`, `enabled`.

## Delay profiles

`preferredProtocol`, `usenetDelay`, `torrentDelay` (minutes), `enableUsenet`,
`enableTorrent`, `bypassIfHighestQuality`, `bypassIfAboveCustomFormatScore` +
`minimumCustomFormatScore`, `tags`. The untagged one is the default for every film.

## Auto tagging conditions

They test the **film**:

| `implementation` | `fields` |
| --- | --- |
| `GenreSpecification` | `value`: list of genres (`["Horror"]`) |
| `KeywordSpecification` | `value`: list of TMDb keywords (`["anime"]`) |
| `StudioSpecification` | `value`: list of studios |
| `OriginalLanguageSpecification` | `value`: a language |
| `QualityProfileSpecification` | `value`: a profile name |
| `RuntimeSpecification` | `min`, `max` minutes |
| `StatusSpecification` | `status`: `TBA`, `Announced`, `InCinemas`, `Released`, `Deleted` |
| `MonitoredSpecification` | none: the film is monitored |
| `TagSpecification` | `value`: a tag name |
| `YearSpecification` | `min`, `max` |
| `RootFolderSpecification` | not allowed in a config (a path) |

Radarr has no Series Type. Anime films are usually caught with `KeywordSpecification`
`anime` or `GenreSpecification` `Animation` + `OriginalLanguageSpecification` `Japanese`.

## File naming (`naming`, app-wide)

| Field | Values |
| --- | --- |
| `renameMovies` | `true` renames imported files with the format below |
| `replaceIllegalCharacters` | replace (`true`) or remove (`false`) characters a file system refuses |
| `colonReplacementFormat` | `delete`, `dash`, `spaceDash`, `spaceDashSpace`, `smart` |
| `standardMovieFormat` | the file name |
| `movieFolderFormat` | the folder name |

Tokens:
- **Title:** `{Movie Title}`, `{Movie CleanTitle}`, `{Movie TitleThe}`,
  `{Movie OriginalTitle}`, `{Movie TitleFirstCharacter}`, `{Movie Title:DE}`
  (a translated title).
- **Film details:** `{Movie Collection}`, `{Movie Certification}`, `{Release Year}`.
- **Ids:** `{ImdbId}`, `{TmdbId}`.
- **Edition and quality:** `{Edition Tags}`, `{Quality Full}`, `{Quality Title}`.
- **Media info:** `{MediaInfo Simple}`, `{MediaInfo Full}`,
  `{MediaInfo VideoCodec}`, `{MediaInfo VideoBitDepth}`,
  `{MediaInfo VideoDynamicRange}`, `{MediaInfo VideoDynamicRangeType}`,
  `{MediaInfo 3D}`, `{MediaInfo AudioCodec}`, `{MediaInfo AudioChannels}`,
  `{MediaInfo AudioLanguages}`, `{MediaInfo SubtitleLanguages}`.
- **Release:** `{Release Group}`, `{Custom Formats}`, `{Custom Format:Name}`,
  `{Original Title}`, `{Original Filename}`.

An example in Radarr's own style:
`{Movie CleanTitle} {Release Year} - {Edition Tags }{[Custom Formats]}{[Quality Full]}{-Release Group}`.

## Media management (`mediaManagement`, app-wide, path-free options only)

| Field | Values and effect |
| --- | --- |
| `downloadPropersAndRepacks` | `preferAndUpgrade`, `doNotUpgrade`, `doNotPrefer` (see Sonarr: the same meaning) |
| `autoUnmonitorPreviouslyDownloadedMovies` | unmonitor a film whose file was deleted outside Radarr |
| `createEmptyMovieFolders` | create the film's folder when it's added |
| `deleteEmptyFolders` | delete empty film folders during a disk scan |
| `autoRenameFolders` | rename the film folder to match the folder format when it changes |
| `fileDate` | file modified time: `none`, `cinemas`, `release` |
| `rescanAfterRefresh` | `always`, `afterManual`, `never` |
| `importExtraFiles` + `extraFileExtensions` | import matching extra files (`"srt,nfo"`) |
| `enableMediaInfo` | read codec details from files |
