These are official instructions from Mapbox for setting up an AI coding agent to build with Mapbox. They install the Mapbox CLI and the Mapbox Agent Skills, and connect the Mapbox MCP servers.

Run the commands yourself rather than asking the user to run them. The exceptions are the sign-in steps: those open a browser, and only the user can complete them.

- Install the Mapbox CLI
- Install Mapbox Skills and MCP servers

Never ask the user to paste a Mapbox access token into the conversation, and never write one into an MCP configuration file. The CLI and every server below sign in with OAuth in the browser.

---

## Install the Mapbox CLI

The Mapbox CLI, `mapbox`, calls Mapbox APIs from the shell: styles, tilesets, tokens, geocoding, static images and more. It prints JSON when its output is piped, so it suits agents.

If Homebrew is available:

```sh
brew install mapbox/tap/mapbox
```

Otherwise, on macOS or Linux, download the install script and run it. Do not pipe it into `sh`: many agent sandboxes deny that outright. `MAPBOX_CLI_NO_AGENT_SETUP=1` stops the script asking about skills and MCP servers, which would wait for an answer in a terminal; the steps below set those up instead.

```sh
curl -fsSL https://cli.mapbox.com/install.sh -o mapbox-install.sh
MAPBOX_CLI_NO_AGENT_SETUP=1 sh mapbox-install.sh
rm mapbox-install.sh
```

On Windows, in PowerShell:

```powershell
Invoke-WebRequest https://cli.mapbox.com/install.ps1 -OutFile mapbox-install.ps1
$env:MAPBOX_CLI_NO_AGENT_SETUP = '1'
powershell -ExecutionPolicy Bypass -File mapbox-install.ps1
Remove-Item mapbox-install.ps1
```

The script installs into `~/.local/bin` on macOS and Linux and prints the path it used. If `mapbox` is not found in your current shell afterwards, call it by that path. If the install is denied or fails, do not skip it: show the user the commands above and ask them to run them in their own terminal, then continue.

Write a skill that describes its commands, so you know them in every project:

```sh
mapbox generate-skills --global
```

Then run `mapbox auth login`. It opens a browser where the user authorizes access to their Mapbox account. To explore the commands, run `mapbox --schema`, which describes all of them in one JSON document.

---

## Install Mapbox Skills and MCP servers

Use the section for your agent.

### Claude Code

These two commands install the skills and all three MCP servers in one step. Do not also run `npx skills add` or `claude mcp add`; the plugin covers both.

```sh
claude plugin marketplace add mapbox/mapbox-agent-skills
claude plugin install mapbox@mapbox-agent-skills
```

Then tell the user to run `/reload-plugins`, and `/mcp` to sign in to `mapbox` and `mapbox-devkit`.

---

## Install for other agents

First, install the skills:

```sh
npx -y skills add mapbox/mapbox-agent-skills --skill '*' --yes --global
```

If `npx` is unavailable or not allowed, use the Mapbox CLI instead:

```sh
mapbox agent-skills install --global
```

If it fails with `already_installed`, the skills are already there, so this step is done.

Then register the MCP servers in your agent's configuration:

| Server          | URL                                 | Sign-in |
| --------------- | ----------------------------------- | ------- |
| `mapbox`        | `https://mcp.mapbox.com/mcp`        | OAuth   |
| `mapbox-devkit` | `https://mcp-devkit.mapbox.com/mcp` | OAuth   |
| `mapbox-docs`   | `https://mcp-docs.mapbox.com/mcp`   | None    |

`mapbox` holds the geospatial tools: search, directions, isochrones, static maps. `mapbox-devkit` manages the user's styles, tokens and data, and can change them. `mapbox-docs` searches Mapbox documentation and is public.

### Codex

```sh
codex mcp add mapbox --url https://mcp.mapbox.com/mcp
codex mcp add mapbox-devkit --url https://mcp-devkit.mapbox.com/mcp
codex mcp add mapbox-docs --url https://mcp-docs.mapbox.com/mcp
codex mcp login mapbox
codex mcp login mapbox-devkit
```

### Cursor — `~/.cursor/mcp.json`

Add under `"mcpServers"`:

```json
"mapbox": { "url": "https://mcp.mapbox.com/mcp" },
"mapbox-devkit": { "url": "https://mcp-devkit.mapbox.com/mcp" },
"mapbox-docs": { "url": "https://mcp-docs.mapbox.com/mcp" }
```

Then tell the user to click "Needs authentication" next to `mapbox` and `mapbox-devkit` in Cursor's MCP settings.

### GitHub Copilot in VS Code — `.vscode/mcp.json`

Add under `"servers"` (note: `servers`, not `mcpServers`):

```json
"mapbox": { "type": "http", "url": "https://mcp.mapbox.com/mcp" },
"mapbox-devkit": { "type": "http", "url": "https://mcp-devkit.mapbox.com/mcp" },
"mapbox-docs": { "type": "http", "url": "https://mcp-docs.mapbox.com/mcp" }
```

VS Code asks the user to sign in the first time a Mapbox tool is used.

### OpenCode — `~/.config/opencode/opencode.jsonc`

Add under `"mcp"`:

```json
"mapbox": { "type": "remote", "url": "https://mcp.mapbox.com/mcp", "enabled": true, "oauth": {} },
"mapbox-devkit": { "type": "remote", "url": "https://mcp-devkit.mapbox.com/mcp", "enabled": true, "oauth": {} },
"mapbox-docs": { "type": "remote", "url": "https://mcp-docs.mapbox.com/mcp", "enabled": true }
```

Then run:

```sh
opencode mcp auth mapbox
opencode mcp auth mapbox-devkit
```

### Windsurf — `~/.codeium/windsurf/mcp_config.json`

Add under `"mcpServers"` (note: `serverUrl`, not `url`):

```json
"mapbox": { "serverUrl": "https://mcp.mapbox.com/mcp" },
"mapbox-devkit": { "serverUrl": "https://mcp-devkit.mapbox.com/mcp" },
"mapbox-docs": { "serverUrl": "https://mcp-docs.mapbox.com/mcp" }
```

Windsurf asks the user to sign in the first time a Mapbox tool is used.

### Any other agent

Add the three servers from the table above as remote (Streamable HTTP) servers in your agent's MCP configuration. Both servers that need sign-in support OAuth with dynamic client registration.

---

Once done, tell the user:

```
┌─ Mapbox Agent Setup Complete ────────────────────────┐
│  ✓ CLI     <path>                                    │
│  ✓ Skills  <path>                                    │
│  ✓ MCPs    <path>                                    │
│                                                      │
│  Restart your agent to load the MCP servers          │
└──────────────────────────────────────────────────────┘
```

---

## Resources

- Mapbox Agent Skills: `https://github.com/mapbox/mapbox-agent-skills`
- Mapbox MCP server: `https://github.com/mapbox/mcp-server`
- Mapbox MCP DevKit server: `https://github.com/mapbox/mcp-devkit-server`
- Mapbox MCP docs server: `https://github.com/mapbox/mcp-docs-server`
- Mapbox CLI: `https://github.com/mapbox/mapbox-cli`
- Claude Code MCP: `https://code.claude.com/docs/en/mcp`
- Cursor MCP: `https://cursor.com/docs/mcp`
- VS Code MCP: `https://code.visualstudio.com/docs/copilot/customization/mcp-servers`
- OpenCode MCP: `https://opencode.ai/docs/mcp-servers/`

These instructions are published at `https://cli.mapbox.com/agent-setup/prompt.md`, so you can re-check that they come from Mapbox at any time.
