# Lidarr (config key `lidarr`)

Music: artists → albums → tracks. Checked against Lidarr 3.1. Everything here
goes under `"apps": {"lidarr": {…}}`. For how the parts work in general, see
`arr-settings.md`. Lidarr differs from the video arrs in three ways:

- quality sizes are **bitrates**;
- it has **metadata profiles**;
- its custom formats can only test the release name, group, size and indexer flags.

Things a config can't set in Lidarr, which the person does by hand:
- which quality and metadata profile an artist uses;
- monitoring, root folders, indexers and download clients.

## Qualities

Lowest to highest, with the groups Lidarr ships:

Unknown,
[Trash Quality Lossy: MP3-8 … MP3-80],
[Poor Quality Lossy: MP3-96, MP3-112, MP3-128, OGG Vorbis Q5, MP3-160],
[Low Quality Lossy: MP3-192, OGG Vorbis Q6, AAC-192, WMA, MP3-224],
[Mid Quality Lossy: OGG Vorbis Q7, MP3-VBR-V2, MP3-256, OGG Vorbis Q8, AAC-256],
[High Quality Lossy: MP3-VBR-V0, AAC-VBR, MP3-320, OGG Vorbis Q9, AAC-320, OGG Vorbis Q10],
[Lossless: FLAC, ALAC, APE, WavPack, FLAC 24bit, ALAC 24bit],
WAV

## Quality profiles

Built-in names: `Any`, `Lossless`, `Standard`.

| Key | Effect in Lidarr |
| --- | --- |
| `qualities` | allowed qualities, best first; groups make members equal |
| `cutoff` | quality/group where quality upgrades stop |
| `upgradeAllowed` | off = the first acceptable release stays |
| `minFormatScore` | releases scoring less are refused |
| `cutoffFormatScore` | score upgrades stop here |
| `formatScores` | custom format name → score |

Lidarr profiles have **no `minUpgradeFormatScore`** and no language.

## Quality sizes

`min` / `preferred` / `max` are an **average bitrate in kbit/s**, not MB per
minute. The release size is checked against bitrate × the album's length. MP3-320 is
around 320. FLAC ships with preferred 895, and 895 kbit/s is about 390 MB an hour.

## Metadata profiles (Lidarr only)

They decide **which releases of an artist Lidarr tracks at all**: which albums show
up, and so which ones it searches for.

```json
"metadataProfiles": [ { "name": "Albums and EPs", "primaryAlbumTypes": ["Album", "EP"],
                        "secondaryAlbumTypes": ["Studio"], "releaseStatuses": ["Official"] } ]
```

| Key | Values |
| --- | --- |
| `primaryAlbumTypes` | `Album`, `EP`, `Single`, `Broadcast`, `Other` |
| `secondaryAlbumTypes` | `Studio` (a plain studio release), `Compilation`, `Soundtrack`, `Spokenword`, `Interview`, `Live`, `Remix`, `DJ-mix`, `Mixtape/Street`, `Demo`, `Audio drama` |
| `releaseStatuses` | `Official`, `Promotion`, `Bootleg`, `Pseudo-Release` |

Lists left out keep the app's values; a new profile starts from Lidarr's
*Standard*. Built-in names: `Standard`, `None`.

## Custom format conditions

Only four types:

| `implementation` | Tests | `fields` |
| --- | --- | --- |
| `ReleaseTitleSpecification` | regex on the release name | `{"value": "<regex>"}` |
| `ReleaseGroupSpecification` | regex on the parsed group | `{"value": "^(GRP)$"}` |
| `SizeSpecification` | size in GB: more than `min`, at most `max` | `{"min": 0, "max": 2}` |
| `IndexerFlagSpecification` | an indexer flag | `Freeleech`, `Halfleech`, `DoubleUpload`, `Internal`, `Scene`, `Freeleech75`, `Freeleech25` |

Format, bitrate and bit depth are qualities in Lidarr, so rank them with the
profile's quality order and groups, not with custom formats. Use custom formats
for things in the name: vinyl rips, WEB, "deluxe", log/cue, a group. Regex
examples, as JSON strings: `"(?<![a-z0-9])(24[ ._-]?bit|hi[ ._-]?res)(?![a-z0-9])"`,
`"(?<![a-z0-9])(vinyl|LP)(?![a-z0-9])"`.

## Release profiles and delay profiles

Release profiles: `required`, `ignored`, `tags`, `enabled`, as in the other arrs.

Delay profiles list their protocols as `items` instead of the
`enableUsenet` / `usenetDelay` keys:

```json
"delayProfiles": [ { "tags": [], "items": [
  { "name": "Usenet", "protocol": "UsenetDownloadProtocol", "allowed": true, "delay": 0 },
  { "name": "Torrent", "protocol": "TorrentDownloadProtocol", "allowed": true, "delay": 120 } ],
  "bypassIfHighestQuality": true } ]
```

`protocol` is Lidarr's own name for it (`UsenetDownloadProtocol`, `TorrentDownloadProtocol`). The first allowed item is the preferred protocol, and `delay` is in minutes.

## Auto tagging conditions

They test the **artist**:

| `implementation` | `fields` |
| --- | --- |
| `GenreSpecification` | `value`: list of genres |
| `MetadataProfileSpecification` | `value`: a metadata profile name |
| `QualityProfileSpecification` | `value`: a quality profile name |
| `StatusSpecification` | `status`: `Continuing`, `Ended`, `Deleted` |
| `MonitoredSpecification` | none: the artist is monitored |
| `TagSpecification` | `value`: a tag name |
| `RootFolderSpecification` | not allowed in a config (a path) |

## File naming (`naming`, app-wide)

| Field | Values |
| --- | --- |
| `renameTracks` | rename imported tracks with the formats below |
| `replaceIllegalCharacters` | replace (`true`) or remove (`false`) |
| `colonReplacementFormat` | `0` delete, `1` dash, `2` space-dash, `3` space-dash-space, `4` smart |
| `standardTrackFormat` | path and name of a track, relative to the artist folder (`/` makes folders) |
| `multiDiscTrackFormat` | the same for multi-disc albums |
| `artistFolderFormat` | the artist folder |

Tokens:
- **Artist:** `{Artist Name}`, `{Artist CleanName}`, `{Artist NameThe}`,
  `{Artist Disambiguation}`, `{Artist Genre}`, `{Artist MbId}`.
- **Album:** `{Album Title}`, `{Album CleanTitle}`, `{Album Type}`,
  `{Album Disambiguation}`, `{Album Genre}`, `{Album MbId}`, `{Release Year}`.
- **Disc:** `{medium:00}`, `{Medium Name}`, `{Medium Format}`.
- **Track:** `{track:00}`, `{Track Title}`, `{Track CleanTitle}`, `{Track ArtistName}`.
- **Quality:** `{Quality Full}`, `{Quality Title}`.
- **Media info:** `{MediaInfo AudioCodec}`, `{MediaInfo AudioChannels}`,
  `{MediaInfo AudioBitRate}`, `{MediaInfo AudioBitsPerSample}`, `{MediaInfo AudioSampleRate}`.
- **Release:** `{Release Group}`, `{Custom Formats}`, `{Original Title}`, `{Original Filename}`.

## Media management (`mediaManagement`, app-wide, path-free options only)

| Field | Values and effect |
| --- | --- |
| `downloadPropersAndRepacks` | `preferAndUpgrade`, `doNotUpgrade`, `doNotPrefer` |
| `allowFingerprinting` | match tracks by audio fingerprint (AcoustID): `never`, `newFiles`, `allFiles`. Better matching, slower imports |
| `watchLibraryForChanges` | rescan when files change on disk |
| `autoUnmonitorPreviouslyDownloadedTracks` | unmonitor tracks whose file was deleted outside Lidarr |
| `createEmptyArtistFolders` | create the artist folder when an artist is added |
| `deleteEmptyFolders` | delete empty artist and album folders during a disk scan |
| `fileDate` | `none`, `albumReleaseDate` |
| `rescanAfterRefresh` | `always`, `afterManual`, `never` |
| `importExtraFiles` + `extraFileExtensions` | import matching extra files (`"cue,log,jpg"`) |
| `enableMediaInfo` | read codec details from files |
