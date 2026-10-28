# Roadmap

What's planned for wranglarr, in the order it's meant to happen. It's a plan,
not a promise: things move as they're built and tried. What has shipped is in the
[release notes](https://github.com/Wranglarrr/Wranglarr/wiki/Release-Notes).

## Next

In this order.

### 1. Blast-radius checks for community configs

Today neither installing nor removing a config looks at the library. A changed
profile applies to every film, series or artist that uses it, and a profile
removed while something still uses it is refused by the app or left behind.
Before an install or a removal, wranglarr will:

- show, for each profile, tag or setting it touches, how many items use it and which;
- offer to move the items on a profile that's about to be removed to another profile;
- hold back a removal that would leave items on a profile that's gone.

This also covers the parts of an app that point at a profile or a tag:
import lists, auto tagging, release and delay profiles.

### 2. Config providers

Add a repository of configs (a Git repository, or any web address with an
index) and pick configs from its list on the Community configs page. When the
repository changes, you see what a newer version would change and decide whether
to take it. A config's `source` URL is where wranglarr looks for its newer
versions. Installs and updates go on the same timeline as today's installs, so
removing one works the same way.

Blast-radius checks come first: an update from a provider is a change you didn't
write yourself, so you see what it reaches before you take it.

## Being considered

- **Versioned API adapters.** One layer between wranglarr and each arr and
  download client. Each operation says what it needs done, and an
  implementation per API version does it. Most calls use "latest". When an
  upstream API changes, the old behaviour is pinned to the versions that need
  it, and the new one becomes latest.
- **An MCP server for assistants.** It would let someone point their own
  assistant at wranglarr. The assistant would read the current setup, explain
  it, and draft a profile, custom format, tag or regex as a
  `wranglarr-config/1` file to check and import. The
  [`arr-configs` skill](skills/arr-configs/SKILL.md) already describes the
  format, the API calls and each arr's options.
