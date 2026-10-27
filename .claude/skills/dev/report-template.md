# Implementation report template

File: `specs/{nr}-{name}-implementation.md`. Language: English. Keep it factual and short.
On a resume, **append** a new section `## Run {n} – {Date}` instead of rewriting earlier runs.

```markdown
# Epic {N} – {Title} · Implementation report

> Specification: `specs/{nr}-{name}-specification.md`
> As of: {Date} · Mode: team / single session

## Run 1 – {Date}

### Result in one sentence
{What the bot can do now, what doesn't work.}

### Acceptance criteria
| AC | Result | Evidence / note |
|---|---|---|
| AC-1.1 | ✅ | Integration test `tests/…` against local test server |
| AC-1.2 | 🎧 | Checked automatically (levels/packets), manual listening check pending |
| AC-1.3 | ❌ | {Symptom, related fix task} |

### Try it yourself
1. {Start the test server, e.g. command/Docker; credentials hint without real passwords}
2. {Write `bot.toml` from `examples/bot.example.toml`, start the bot}
3. {Steps}

### Completed tasks
| Task | Title | Owner |
|---|---|---|

### Deviations from the specification
{What, why, who approved – or "None".}

### Open points & known issues
{List – or "None".}

### Technical notes
{New dependencies, changed local data formats/migrations, required environment variables/secrets (names only), build/setup steps, tested platforms and server versions.}
```
