# The wranglarr-config/1 format

One JSON object per job. The authoritative description is the wranglarr wiki
page *Community Configs → The format*; this is the working summary.

```json
{
  "wranglarr": "wranglarr-config/1",
  "name": "Anime, English dub only",
  "description": "What it does, and what to do after importing (set Series Type to Anime, switch the profile).",
  "author": "optional",
  "link": "https://optional.example/where-it-came-from",
  "source": { "name": "optional: who it's from", "url": "https://optional.example/this-config.json" },
  "apps": {
    "sonarr": {
      "customFormats": [
        {
          "name": "English audio",
          "includeCustomFormatWhenRenaming": false,
          "specifications": [
            { "name": "Dubbed", "implementation": "ReleaseTitleSpecification", "negate": false, "required": false,
              "fields": { "value": "(?<![a-z0-9])(dub(bed|s)?|eng(lish)?[ ._-]?(dub(bed)?|audio))(?![a-z0-9])" } }
          ]
        },
        {
          "name": "10bit",
          "extra": { "default": true, "note": "Switch off if a player can't do 10-bit HEVC" },
          "specifications": [
            { "name": "10bit", "implementation": "ReleaseTitleSpecification", "fields": { "value": "10[ ._-]?bit" } }
          ]
        }
      ],
      "qualityProfiles": [
        {
          "name": "Anime, English dub",
          "upgradeAllowed": true,
          "qualities": [
            { "name": "1080p", "qualities": ["Bluray-1080p", "WEBDL-1080p", "WEBRip-1080p", "HDTV-1080p"] },
            { "name": "720p", "qualities": ["Bluray-720p", "WEBDL-720p", "WEBRip-720p", "HDTV-720p"] }
          ],
          "cutoff": "1080p",
          "minFormatScore": 100, "cutoffFormatScore": 100, "minUpgradeFormatScore": 1,
          "formatScores": { "English audio": 100, "10bit": 5 }
        }
      ],
      "qualityDefinitions": { "HDTV-720p": { "min": 1, "preferred": null, "max": null } },
      "releaseProfiles": [ { "name": "No cams", "enabled": true, "required": [], "ignored": ["HDCAM", "TELESYNC"], "tags": [] } ],
      "delayProfiles": [ { "tags": ["anime"], "preferredProtocol": "torrent", "usenetDelay": 0, "torrentDelay": 60 } ],
      "tags": ["anime"],
      "autoTags": [
        { "name": "Anime series", "tags": ["anime"], "removeTagsAutomatically": false,
          "specifications": [ { "name": "anime", "implementation": "SeriesTypeSpecification", "fields": { "value": "Anime" } } ] }
      ],
      "naming": { "renameEpisodes": true },
      "mediaManagement": { "downloadPropersAndRepacks": "doNotPrefer" }
    }
  }
}
```

## Keys

| Key | Notes |
| --- | --- |
| `wranglarr` | always `"wranglarr-config/1"` |
| `name` | required, up to 100 characters. `description` up to 4000, `author` up to 100, `link` http(s) |
| `source` | optional: a name (up to 100), a URL to the config's JSON (http(s), up to 500), or both as `{"name", "url"}`. A bare string is the URL when it starts with http(s)://, else the name |
| `apps` | one part per kind: `radarr`, `sonarr`, `lidarr`, `whisparr`. At import, the person picks which instance(s) of that kind each part goes to |
| `customFormats` | the arrs' own Export shape, so formats from an arr or TRaSH Guides paste in as they are. `fields` is `{"value": …}` (or `{"min","max"}`), or the API's `[{"name","value"}]`. `negate` and `required` default to false |
| `qualityProfiles` | see arr-settings.md. `qualities` best first, groups as `{"name","qualities"}`. `cutoff` names a quality or group (default: the first). Keys left out keep the app's value when the profile exists. `formatScores` sets only the formats it lists |
| `qualityDefinitions` | quality name → any of `min`, `preferred`, `max` (`null` = no limit): MB per minute of runtime, or kbit/s in Lidarr. App-wide |
| `releaseProfiles` | `name`, `enabled`, `required`, `ignored`, `tags` (names) |
| `delayProfiles` | matched by `tags` (none = the default profile). `enableUsenet`, `enableTorrent`, `preferredProtocol`, `usenetDelay`, `torrentDelay`, `bypassIfHighestQuality`, `bypassIfAboveCustomFormatScore`, `minimumCustomFormatScore`. Lidarr uses `items` instead (see lidarr.md) |
| `tags` | tag names to create (lower-case, digits, dashes). Tags named anywhere else are created too |
| `autoTags` | `name`, `tags`, `removeTagsAutomatically`, `specifications`. Profiles, tags and choices by name. No root-folder conditions |
| `metadataProfiles` | Lidarr: `name`, `primaryAlbumTypes`, `secondaryAlbumTypes`, `releaseStatuses` by name |
| `naming` | the API's naming field names. App-wide |
| `mediaManagement` | path-free options only. App-wide |
| `extra` | on a custom format, quality or metadata profile, release profile or auto tagging rule: `{"default": true|false, "note": "≤300 chars"}` makes it optional at import. Switching off a format also drops the scores profiles give it |

Rules the importer enforces:

- Names are unique within a list.
- Everything is matched **by name**: an existing item with the same name is
  changed, otherwise it's added. Nothing else in the app is touched.
- Quality names are the app's own and differ between apps (Radarr
  `Remux-1080p`, Sonarr `Bluray-1080p Remux`).
- At most 512 KB.

## Design checklist

- [ ] New items have names of their own (a shared prefix), not built-in names.
- [ ] Every format a profile scores is in the config or already in the app.
- [ ] Each quality profile's order and groups say what should win, and the scores say the rest.
- [ ] `minFormatScore` / `cutoffFormatScore` / `minUpgradeFormatScore` are set on purpose.
- [ ] App-wide parts are only included when the job needs them.
- [ ] Regex tested against names that should match and names that shouldn't.
- [ ] The description says what to do after importing.
- [ ] `/api/configs/check` shows no *problem* rows.
