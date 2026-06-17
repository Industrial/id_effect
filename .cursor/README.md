# Cursor setup

## Agency agents (submodule)

The [agency-agents](https://github.com/msitarzewski/agency-agents) repo is included as a git submodule at `.cursor/agency-agents`. It provides agent definitions that can be converted into Cursor rules.

### Fresh clone

After cloning the monorepo, init the submodule once:

```bash
git submodule update --init --recursive
```

### Auto-updates

**Dependabot** is configured to open a PR whenever the upstream `agency-agents` repo has new commits (weekly check). Merge the Dependabot PR to update the submodule; no manual fetching needed.

### Regenerating Cursor rules from agency-agents

To (re)generate `.cursor/rules/*.mdc` from the submodule:

```bash
.cursor/agency-agents/scripts/install.sh --tool cursor
```

Run from the monorepo root. See `.cursor/agency-agents/integrations/cursor/README.md` for details.
