# Workflows

A workflow is a named, multi-step recipe of `mapbox` commands and scripts. None ships inside the binary: `mapbox workflow install` copies one in, and `mapbox workflow run` runs only what is installed. See [docs/commands.md](../docs/commands.md#workflows) for the commands.

> **Beta and in development. Not recommended for use.** The `workflow` command, the format below (`version: 1`) and the published workflows may change or be removed without notice. Every `mapbox workflow` subcommand says so on stderr.

## Layout

```
workflow/
  README.md                # this file
  copy-style/              # the workflow's name
    workflow.yaml          # required
    scripts/               # the scripts its steps run
      prepare.py
    README.md              # optional
```

- The directory name is the workflow's name: lower-case letters, digits and dashes, and the same as `name` in `workflow.yaml`.
- A workflow holds `workflow.yaml`, `README.md` and `scripts/`, and nothing else.
- Every file in `scripts/` is run by some step. A workflow ships only the scripts it runs.

The same rules apply to any workflow `install` reads, from this repository, another one (`--repo OWNER/REPO`, laid out the same way) or a local directory. `every_published_workflow_is_valid` in `src/workflow/mod.rs` holds this directory to them in `cargo test`.

## `workflow.yaml`

```yaml
version: 1                        # required; the schema version
name: copy-style                  # required; the directory's name
summary: Copy a style from one account to another   # required; one line
description: |                    # optional; shown by `workflow show`
  Longer text.

inputs:                           # optional; given as --input KEY=VALUE
  style_id:
    type: string                  # string | number | boolean
    required: true
    description: ID of the style to copy
  name:
    type: string                  # not required and no default: null
  zoom:
    type: number
    default: 12

steps:                            # required; run in order, first failure stops
  - id: source                    # required; lower-case, digits, underscores
    name: Read the style          # optional; the progress line
    command: styles get           # a mapbox command
    args:                         # names as `mapbox --schema` lists them
      style-id: ${{ inputs.style_id }}
      profile: work               # global options too
      use-login: true             # a flag takes true or false

  - id: body
    script: prepare.py            # a file in scripts/
    interpreter: python3          # optional for .sh, .py and .js
    args: ["--zoom", "${{ inputs.zoom }}"]
    stdin:                        # optional; sent to the step as JSON
      style: ${{ steps.source.output }}

outputs:                          # optional; the default is the last step's output
  id: ${{ steps.source.output.id }}
```

A step is a `command` or a `script`, never both.

### `description`

`workflow show` renders it in a fixed layout, after the name and summary and before the inputs and steps. It reads a small subset of Markdown, so every workflow's page looks the same:

- A blank line separates paragraphs. A paragraph is reflowed to 80 columns, so line breaks inside one do not matter.
- A line indented by two or more spaces is a command. It is highlighted and kept exactly as written.
- A line starting with `- ` is a list item. An indented line under an item continues the item.
- `code` spans are highlighted.

Anything else is shown as a plain paragraph. There is no other formatting: no headings, links or emphasis.

### Expressions

`${{ inputs.<name> }}` and `${{ steps.<id>.output }}`, followed by any number of `.key` and `[index]`. Nothing else: no operators and no functions. Logic belongs in a script.

- A value that is exactly one expression keeps its JSON type, so a whole object can go to `stdin` and a number stays a number.
- An expression inside a longer string is written in as text. One that is null there is an error rather than an empty string.
- An expression may name only a declared input or an earlier step. That is checked before any step runs.

### Command steps

Each command step runs this `mapbox` binary again, with `--output json`, and its JSON result becomes the step's output. It resolves its token, timeouts and path encoding exactly as the same command typed by hand would, and appears in `mapbox history` on its own.

- `args` keys are the argument names `mapbox --schema <command>` lists: a flag's long name, or a positional's name. Global options such as `profile`, `username` and `use-login` are accepted too.
- `output`, `quiet`, `schema` and `dry-run` belong to the runner. `token` is refused, because a token written into a workflow is a secret in a file: log in under a profile and name the `profile` instead.
- Global options given to `mapbox workflow run` (`--profile`, `--username`, `--use-login`, `--timeout`, `--yes`, `--debug`, `--token`) reach every command step that does not set its own.
- `stdin` is sent to the command, for `--data @-`. Without it, the command reads the terminal, which is where a confirmation prompt gets its answer.
- A step cannot run `mapbox workflow`.

### Script steps

A script runs from the installed copy of `scripts/`, in the directory `mapbox workflow run` was started from.

- It gets `args` as its arguments and `stdin` on standard input, as JSON unless the value is a string. With no `stdin` it reads nothing.
- Whatever it writes to stdout is its output: JSON when it parses as JSON, the text otherwise. It writes progress to stderr.
- A non-zero exit stops the workflow.
- `MAPBOX_CLI` is the path to this `mapbox` binary, for a script that runs commands of its own. `MAPBOX_WORKFLOW_ROOT` is the installed workflow's directory.
- The interpreter must be installed on the machine. `.sh` runs under `sh`, `.py` under `python3` and `.js` under `node`, and any other extension needs `interpreter`.
