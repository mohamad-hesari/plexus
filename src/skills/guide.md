# Plexus skills

Plexus ships instructions for AI agents inside its binary, so they always match the installed
version. They work with any agent harness: read them on demand, or save them wherever your
harness keeps skills.

```bash
{{plexus}} skills list            # slugs and one-line descriptions
{{plexus}} skills get <slug>      # print one skill or reference to stdout
```

- Top-level slugs (like `client`) print a complete `SKILL.md`, with `name` and `description`
  frontmatter. To store one, save the output as `<your skills dir>/plexus-<slug>/SKILL.md`.
- Slugs with a `/` (like `client/forwarding`) are reference docs. A skill links to them by
  command (`{{plexus}} skills get client/forwarding`), so links work whether you store the
  text or fetch it each time.
- If `plexus` is not on PATH (a project-local install), pass the command you use instead:
  `plexus skills get client --cmd "pnpm exec plexus"`. Every command in the text uses it.

## Without storing anything

An agent only looks for a skill it knows exists. If you don't store the skills, add this line
to `AGENTS.md`, `CLAUDE.md`, or whatever file your agent reads at the start of a session:

```
A plexus watch session may be running in this repo; before running builds, type checks, lints or dev servers yourself, run `{{plexus}} skills get client` and follow it.
```
