# Skills

Instructions and reference material that let an AI assistant work with a
wranglarr stack.

| Skill | What it's for |
| --- | --- |
| [arr-configs](arr-configs/SKILL.md) | understand the arr settings you have, and write [community configs](../wiki/Community-Configs.md) (custom formats, profiles, scores, tags, regex) for a job you describe |

A skill is a folder with a `SKILL.md` (when to use it and how) and
`references/` that the assistant reads as needed.

## Using one

- **Claude Code**: copy the folder into `~/.claude/skills/` (for you) or
  `.claude/skills/` in a project.
- **Claude apps**: zip the folder and add it under Settings → Capabilities → Skills.
- **Any other assistant**: give it `SKILL.md` and the files in `references/`
  as context.

To read your setup and test configs, the assistant needs wranglarr's address
and an API key (Account → API keys). Everything it does with the key goes
through wranglarr's API. Only *apply* and *undo* change your apps, and the
skill tells it to do those only when you ask.

The same material is meant to back a wranglarr MCP server later, so an
assistant can call those endpoints as tools directly.
