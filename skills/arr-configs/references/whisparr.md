# Whisparr (config key `whisparr`)

Adult content. Not checked against a live Whisparr: its settings follow the
app it was forked from, so read the person's inventory first
(`GET /api/configs/inventory/<app>`) to see which kind they run.

| Whisparr | Built on | Library | Use the rules in |
| --- | --- | --- | --- |
| v3 | Radarr | scenes and films as *movies* | `radarr.md` |
| v2 | Sonarr | sites as *series*, scenes as *episodes* | `sonarr.md` |

What carries over, and what to check:

- **Qualities, quality profiles, custom format conditions, quality sizes (MB
  per minute):** as the base app. Quality names come from the base app (v3:
  `Remux-1080p`, v2: `Bluray-1080p Remux`). Check them in the inventory's
  quality sizes.
- **Built-in profile names:** the base app's (`Any`, `SD`, `HD-720p`,
  `HD-1080p`, `Ultra-HD`, `HD - 720p/1080p`).
- **Conditions that only exist in the base app:** Radarr's
  `EditionSpecification`, `YearSpecification` and `QualityModifierSpecification`,
  Sonarr's `ReleaseTypeSpecification`. Use them only when the inventory shows
  that base. `/api/configs/check` reports a type the app doesn't have.
- **Naming and media management field names:** the base app's
  (`standardMovieFormat` vs `standardEpisodeFormat`). Export the current
  naming first and keep the field names it shows.
- **Release names:** these usually follow `Site.YY.MM.DD.Performer.Scene.Title.1080p`.
  Write regex for that shape, and test real names with `/api/configs/try`.
