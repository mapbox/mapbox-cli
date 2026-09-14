//! Where an Agent Skill goes.
//!
//! A coding agent that reads skills from disk looks in two places: a directory
//! inside the repository, which travels with the project and belongs in its
//! history, and one under the user's home, which applies to every project on
//! the machine. That is two answers per agent, plus `--dir` for a caller who
//! wants neither.
//!
//! Kept apart from the commands that write skills — [`crate::generate_skills`]
//! and [`crate::agent_skills`] — deliberately. Nothing here knows what a skill
//! contains, and neither of them knows about `CLAUDE_CONFIG_DIR`, so a third
//! writer or a sixteenth agent is a change to one file rather than three.
//!
//! # The table is the module
//!
//! [`SPECS`] is read from other tools' documentation, not derived from
//! anything, so the only thing that can keep it honest is saying each string
//! once and pinning it. `the_table_is_internally_consistent` and
//! `each_agents_paths_are_the_ones_it_reads` below do that.
//!
//! Two facts about the ecosystem shape it:
//!
//! - **`.agents/skills` is the converging project-level convention** — Codex,
//!   Cursor, Cline, Gemini CLI, GitHub Copilot, Zed, OpenCode and Amp all read
//!   it, and more arrive every month. Claude Code is the notable holdout with
//!   `.claude/skills`. So at project level these fifteen agents resolve to
//!   eight directories rather than fifteen — eight of them share one — and
//!   adding an agent that follows the convention costs a row and no new
//!   destination.
//! - **Global directories do not converge at all.** Each agent has its own,
//!   and they are not uniform: some hang off `$HOME`, some off
//!   `$XDG_CONFIG_HOME`, two are relocatable by their own variable, and Cline
//!   and Zed put theirs in the shared `~/.agents/skills`. Hence [`Location`]
//!   rather than a single "home directory" string — the directory that means
//!   *installed* and the directory skills go *into* are separate questions,
//!   and for three of the fifteen (Amp, Cline and Zed) they have different
//!   answers. The other twelve, Windsurf included, put their skills directly
//!   under the directory that detects them.
//!
//! The roster is a curated subset of the ~79 agents
//! [vercel-labs/skills](https://github.com/vercel-labs/skills) tracks, not a
//! mirror of it: that list changes weekly, a third of its entries need
//! detection rules nothing here could exercise, and `--dir` already covers
//! every agent this table has never heard of. Adding one is a row in [`SPECS`]
//! and a line in the test.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, ArgMatches};

/// `--agent`, `--global` and `--dir`: the arg ids, and their long spellings.
pub const AGENT_ARG: &str = "agent";
pub const GLOBAL_ARG: &str = "global";
pub const DIR_ARG: &str = "dir";

/// What a path in [`SPECS`] hangs off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Base {
    /// `$HOME`.
    Home,
    /// `$XDG_CONFIG_HOME`, or `$HOME/.config` when that is unset — on every
    /// platform, macOS included. That is deliberately not `dirs::config_dir()`,
    /// which answers `~/Library/Application Support` on macOS: the agents that
    /// use this base (OpenCode, Amp, Goose, Zed) read `~/.config` there, and a
    /// skill written to the platform-idiomatic directory would be a correct
    /// file somewhere nothing looks.
    XdgConfig,
}

/// A directory, as a base and a path relative to it.
#[derive(Clone, Copy, Debug)]
struct Location {
    base: Base,
    path: &'static str,
}

/// Everything this module knows about one agent.
#[derive(Debug)]
struct Spec {
    /// How the agent is written as an `--agent` value.
    flag: &'static str,
    /// How the agent is named in prose.
    label: &'static str,
    /// Where it reads skills from inside a project, relative to the directory
    /// the command runs in.
    project_skills_dir: &'static str,
    /// A variable that relocates the agent's whole directory. When it is set,
    /// it replaces both [`Spec::home`] and the base of [`Spec::global`].
    home_env: Option<&'static str>,
    /// The directory whose existence means the agent is installed. Presence of
    /// a directory, not of a binary: a `PATH` lookup would miss an agent
    /// installed under another name and find one that has never been run.
    home: Location,
    /// Where it reads skills that apply to every project.
    global: Location,
}

/// Every agent `--agent` accepts, in the order they are offered and listed.
///
/// Claude Code and Codex first because they were here first and the help text
/// reads better for it; the rest alphabetically. Each row is four strings read
/// from that agent's own documentation — a typo writes a correct skill into a
/// directory nothing reads, which is why every one of them is asserted below.
static SPECS: &[Spec] = &[
    Spec {
        flag: "claude-code",
        label: "Claude Code",
        project_skills_dir: ".claude/skills",
        home_env: Some("CLAUDE_CONFIG_DIR"),
        home: Location {
            base: Base::Home,
            path: ".claude",
        },
        global: Location {
            base: Base::Home,
            path: ".claude/skills",
        },
    },
    Spec {
        flag: "codex",
        label: "Codex",
        project_skills_dir: ".agents/skills",
        home_env: Some("CODEX_HOME"),
        home: Location {
            base: Base::Home,
            path: ".codex",
        },
        global: Location {
            base: Base::Home,
            path: ".codex/skills",
        },
    },
    Spec {
        flag: "amp",
        label: "Amp",
        project_skills_dir: ".agents/skills",
        home_env: None,
        home: Location {
            base: Base::XdgConfig,
            path: "amp",
        },
        // Not `amp/skills`: Amp reads the shared directory under the config
        // home, the way it reads the shared one in a project.
        global: Location {
            base: Base::XdgConfig,
            path: "agents/skills",
        },
    },
    Spec {
        flag: "cline",
        label: "Cline",
        project_skills_dir: ".agents/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".cline",
        },
        // Detected by `~/.cline`, but its skills go in the shared directory.
        global: Location {
            base: Base::Home,
            path: ".agents/skills",
        },
    },
    Spec {
        flag: "continue",
        label: "Continue",
        project_skills_dir: ".continue/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".continue",
        },
        global: Location {
            base: Base::Home,
            path: ".continue/skills",
        },
    },
    Spec {
        flag: "cursor",
        label: "Cursor",
        project_skills_dir: ".agents/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".cursor",
        },
        global: Location {
            base: Base::Home,
            path: ".cursor/skills",
        },
    },
    Spec {
        flag: "gemini-cli",
        label: "Gemini CLI",
        project_skills_dir: ".agents/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".gemini",
        },
        global: Location {
            base: Base::Home,
            path: ".gemini/skills",
        },
    },
    Spec {
        flag: "github-copilot",
        label: "GitHub Copilot",
        project_skills_dir: ".agents/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".copilot",
        },
        global: Location {
            base: Base::Home,
            path: ".copilot/skills",
        },
    },
    Spec {
        flag: "goose",
        label: "Goose",
        project_skills_dir: ".goose/skills",
        home_env: None,
        home: Location {
            base: Base::XdgConfig,
            path: "goose",
        },
        global: Location {
            base: Base::XdgConfig,
            path: "goose/skills",
        },
    },
    Spec {
        flag: "kiro-cli",
        label: "Kiro CLI",
        project_skills_dir: ".kiro/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".kiro",
        },
        global: Location {
            base: Base::Home,
            path: ".kiro/skills",
        },
    },
    Spec {
        flag: "opencode",
        label: "OpenCode",
        project_skills_dir: ".agents/skills",
        home_env: None,
        home: Location {
            base: Base::XdgConfig,
            path: "opencode",
        },
        global: Location {
            base: Base::XdgConfig,
            path: "opencode/skills",
        },
    },
    Spec {
        flag: "qwen-code",
        label: "Qwen Code",
        project_skills_dir: ".qwen/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".qwen",
        },
        global: Location {
            base: Base::Home,
            path: ".qwen/skills",
        },
    },
    Spec {
        flag: "roo",
        label: "Roo Code",
        project_skills_dir: ".roo/skills",
        home_env: None,
        home: Location {
            base: Base::Home,
            path: ".roo",
        },
        global: Location {
            base: Base::Home,
            path: ".roo/skills",
        },
    },
    Spec {
        flag: "windsurf",
        label: "Windsurf",
        project_skills_dir: ".windsurf/skills",
        home_env: None,
        // Windsurf is Codeium's, and its directory still says so.
        home: Location {
            base: Base::Home,
            path: ".codeium/windsurf",
        },
        global: Location {
            base: Base::Home,
            path: ".codeium/windsurf/skills",
        },
    },
    Spec {
        flag: "zed",
        label: "Zed",
        project_skills_dir: ".agents/skills",
        home_env: None,
        // Zed's own `config_dir()` also looks at `%APPDATA%` on Windows and
        // `$FLATPAK_XDG_CONFIG_HOME` under Flatpak. Neither is followed here:
        // the cost of missing them is that `--agent zed` has to be written out
        // rather than detected, and `--dir` covers the rest.
        home: Location {
            base: Base::XdgConfig,
            path: "zed",
        },
        // Detected under the config home, but its skills go in the shared
        // directory under `$HOME`.
        global: Location {
            base: Base::Home,
            path: ".agents/skills",
        },
    },
];

/// One agent, as an index into [`SPECS`].
///
/// A newtype rather than a fifteen-variant enum: every method would otherwise
/// be a fifteen-arm match, and the strings would be spread over five of them
/// instead of sitting in one row where a reader can check them against the
/// agent's documentation.
///
/// Ordered by table position, which is what `resolve` sorts by, so the order
/// destinations are reported in is the order [`SPECS`] is written in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Agent(usize);

impl Agent {
    /// Every agent, in table order.
    pub fn all() -> impl Iterator<Item = Agent> + Clone {
        (0..SPECS.len()).map(Agent)
    }

    fn spec(self) -> &'static Spec {
        &SPECS[self.0]
    }

    /// How the agent is written as an `--agent` value.
    pub fn flag(self) -> &'static str {
        self.spec().flag
    }

    /// How the agent is named in prose.
    pub fn label(self) -> &'static str {
        self.spec().label
    }

    /// Where the agent looks for skills inside a project, relative to the
    /// directory the command runs in.
    pub fn project_skills_dir(self) -> &'static str {
        self.spec().project_skills_dir
    }

    /// The variable that relocates the agent's directory, for the two agents
    /// that have one.
    pub fn home_env(self) -> Option<&'static str> {
        self.spec().home_env
    }

    fn from_flag(value: &str) -> Option<Agent> {
        Agent::all().find(|agent| agent.flag() == value)
    }
}

/// `$XDG_CONFIG_HOME`, or `$HOME/.config`. See [`Base::XdgConfig`].
fn xdg_config_home() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => dirs::home_dir().map(|home| home.join(".config")),
    }
}

fn resolve_location(location: Location) -> Option<PathBuf> {
    let base = match location.base {
        Base::Home => dirs::home_dir(),
        Base::XdgConfig => xdg_config_home(),
    }?;
    Some(base.join(location.path))
}

/// Where each agent lives on this machine, and where its global skills go.
///
/// Resolved once and passed around rather than read at each use, so a test can
/// describe a machine without touching the process environment — which is
/// shared, and these tests run in parallel.
#[derive(Clone, Debug, Default)]
pub struct AgentHomes {
    /// Indexed by [`Agent`]'s index; empty means "nothing resolved", which
    /// [`AgentHomes::default`] produces and which reads as no agent installed.
    entries: Vec<Paths>,
}

/// One agent's two directories. They are separate because for four of the
/// fifteen they are in different places — see the module docs.
#[derive(Clone, Debug, Default)]
struct Paths {
    /// Its existence means the agent is installed.
    home: Option<PathBuf>,
    /// Where `--global` writes.
    global: Option<PathBuf>,
}

impl AgentHomes {
    /// The real machine.
    ///
    /// An empty relocation variable is treated as unset. `export CODEX_HOME=`
    /// is how a shell clears one, and reading it literally would resolve the
    /// skills directory to `skills` in the current directory — a write nobody
    /// asked for, in a place nobody would look.
    pub fn from_env() -> Self {
        let entries = Agent::all()
            .map(|agent| {
                let relocated = agent
                    .home_env()
                    .and_then(std::env::var_os)
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from);

                match relocated {
                    // The variable moves the whole directory, skills included.
                    Some(home) => Paths {
                        global: Some(home.join("skills")),
                        home: Some(home),
                    },
                    None => Paths {
                        home: resolve_location(agent.spec().home),
                        global: resolve_location(agent.spec().global),
                    },
                }
            })
            .collect();

        AgentHomes { entries }
    }

    /// A machine described rather than probed for, so a test can arrange one
    /// without touching the process environment. Each named agent is present
    /// at `home`, with its global skills directory underneath it — the shape
    /// the relocation variables produce.
    #[cfg(test)]
    pub fn describing(present: &[(Agent, PathBuf)]) -> Self {
        let entries = Agent::all()
            .map(|agent| match present.iter().find(|(a, _)| *a == agent) {
                Some((_, home)) => Paths {
                    global: Some(home.join("skills")),
                    home: Some(home.clone()),
                },
                None => Paths::default(),
            })
            .collect();
        AgentHomes { entries }
    }

    fn paths(&self, agent: Agent) -> Option<&Paths> {
        self.entries.get(agent.0)
    }

    fn home(&self, agent: Agent) -> Option<&Path> {
        self.paths(agent)?.home.as_deref()
    }

    /// The agents this machine appears to have: the ones whose directory is
    /// there.
    pub fn detected(&self) -> Vec<Agent> {
        Agent::all()
            .filter(|agent| self.home(*agent).is_some_and(Path::exists))
            .collect()
    }

    /// Where the agent reads skills that apply to every project.
    pub fn global_skills_dir(&self, agent: Agent) -> Result<PathBuf> {
        self.paths(agent)
            .and_then(|paths| paths.global.clone())
            .ok_or_else(|| match agent.home_env() {
                Some(variable) => anyhow!(
                    "Cannot find {}'s home directory. Set {variable} to it, or pass --dir.",
                    agent.label()
                ),
                None => anyhow!(
                    "Cannot find {}'s home directory, and it has no variable that names one. \
                     Pass --dir instead.",
                    agent.label()
                ),
            })
    }
}

/// A directory to write a skill into, and why it was chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub root: PathBuf,
    /// What picked this — `Claude Code, project` — for the line that reports
    /// the write. `None` for `--dir`, where the caller already knows.
    pub source: Option<String>,
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(source) => write!(f, "{} ({source})", self.root.display()),
            None => write!(f, "{}", self.root.display()),
        }
    }
}

/// The three flags that choose a destination, for a command to add to its own
/// arguments.
///
/// `--dir` conflicts with both of the others rather than quietly outranking
/// them: `mapbox generate-skills --global --dir ./out` is a caller who means
/// two different things, and picking one of them is a worse answer than
/// saying so.
pub fn args() -> Vec<Arg> {
    let flags: Vec<&'static str> = Agent::all().map(Agent::flag).collect();
    vec![
        Arg::new(AGENT_ARG)
            .long(AGENT_ARG)
            .value_name("AGENT")
            .action(ArgAction::Append)
            .value_parser(PossibleValuesParser::new(flags))
            .conflicts_with(DIR_ARG)
            .help(
                "Agent to write skills for, repeatable. Defaults to whichever \
                 agents are installed",
            ),
        Arg::new(GLOBAL_ARG)
            .long(GLOBAL_ARG)
            .action(ArgAction::SetTrue)
            .conflicts_with(DIR_ARG)
            .help("Write to the agent's home directory, for every project, rather than this one"),
        Arg::new(DIR_ARG)
            .long(DIR_ARG)
            .value_name("DIR")
            .value_parser(clap::value_parser!(PathBuf))
            .help("Write to this directory instead, and nowhere else"),
    ]
}

/// Every directory to write into, in a fixed order and each named once.
///
/// Two directories are the same directory when they are written the same way.
/// Paths are compared as given rather than canonicalised: canonicalising
/// requires the path to exist, and the interesting case here is a directory
/// this command is about to create. The cost is that `--dir a` and
/// `--dir ./a` would be treated as two — which no caller writes, and which
/// costs a redundant write rather than a wrong one.
///
/// The dedup earns its keep now that most agents share `.agents/skills`:
/// `--agent codex --agent cursor --agent zed` is one write, not three.
pub fn resolve(
    dir: Option<&Path>,
    requested: &[Agent],
    global: bool,
    homes: &AgentHomes,
    require: bool,
) -> Result<Vec<Destination>> {
    if let Some(dir) = dir {
        return Ok(vec![Destination {
            root: dir.to_path_buf(),
            source: None,
        }]);
    }

    let mut agents: Vec<Agent> = if requested.is_empty() {
        homes.detected()
    } else {
        requested.to_vec()
    };
    agents.sort_unstable();
    agents.dedup();

    let mut out: Vec<Destination> = vec![];
    for agent in agents {
        let (root, source) = if global {
            (
                homes.global_skills_dir(agent)?,
                format!("{}, all projects", agent.label()),
            )
        } else {
            (
                PathBuf::from(agent.project_skills_dir()),
                format!("{}, this project", agent.label()),
            )
        };
        // The label of the agent that got there first wins. With eight agents
        // sharing `.agents/skills` the alternative — naming all of them — is a
        // line nobody reads on a command that wrote one directory.
        if out.iter().any(|existing| existing.root == root) {
            continue;
        }
        out.push(Destination {
            root,
            source: Some(source),
        });
    }

    if require && out.is_empty() {
        let known: Vec<&str> = Agent::all().map(Agent::flag).collect();
        return Err(anyhow!(
            "Nowhere to write skills: no --agent was named, and no agent's home \
             directory was found. Pass --agent to write for one anyway ({}), \
             --global to write under its home directory, or --dir to write \
             somewhere specific.",
            known.join(", ")
        ));
    }

    Ok(out)
}

/// [`resolve`], off a parsed command line that declared [`args`].
pub fn from_matches(
    matches: &ArgMatches,
    homes: &AgentHomes,
    require: bool,
) -> Result<Vec<Destination>> {
    let dir = matches.get_one::<PathBuf>(DIR_ARG).map(PathBuf::as_path);
    // Clap has already refused anything that is not one of the known
    // spellings, so an unrecognised value here is impossible rather than
    // ignored.
    let requested: Vec<Agent> = matches
        .get_many::<String>(AGENT_ARG)
        .into_iter()
        .flatten()
        .filter_map(|value| Agent::from_flag(value))
        .collect();

    resolve(
        dir,
        &requested,
        matches.get_flag(GLOBAL_ARG),
        homes,
        require,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn agent(flag: &str) -> Agent {
        Agent::from_flag(flag).unwrap_or_else(|| panic!("no agent called `{flag}`"))
    }

    /// A directory of this test's own, created and returned. Named after the
    /// caller so two tests never share one, and left behind — it holds
    /// nothing, and `CARGO_TARGET_TMPDIR` is not set for unit tests.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mapbox-cli-skill-dest-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Every string this module reads from another tool's documentation.
    ///
    /// Written out rather than derived: they are the whole of the coupling to
    /// fifteen other projects, and a typo in one of them writes a correct
    /// skill into a directory nothing reads. Changing one has to be deliberate
    /// enough to change it here too.
    ///
    /// The `home`/`global` columns are the ones worth reading carefully: for
    /// Amp, Cline, Windsurf and Zed they are not the same directory, which is
    /// the whole reason [`Spec`] carries both.
    /// One row of the table as a test reads it: flag, label, project
    /// directory, relocation variable, home, global. Named because the tuple
    /// is six wide, and six-wide tuples are how a column ends up compared
    /// against the wrong one.
    type Row = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
        String,
        String,
    );

    #[test]
    fn each_agents_paths_are_the_ones_it_reads() {
        // (flag, label, project dir, home env, home, global)
        // The same six columns, as written out below with `&str` paths.
        type Written = (
            &'static str,
            &'static str,
            &'static str,
            Option<&'static str>,
            &'static str,
            &'static str,
        );
        let expected: &[Written] = &[
            (
                "claude-code",
                "Claude Code",
                ".claude/skills",
                Some("CLAUDE_CONFIG_DIR"),
                "$HOME/.claude",
                "$HOME/.claude/skills",
            ),
            (
                "codex",
                "Codex",
                ".agents/skills",
                Some("CODEX_HOME"),
                "$HOME/.codex",
                "$HOME/.codex/skills",
            ),
            (
                "amp",
                "Amp",
                ".agents/skills",
                None,
                "$XDG/amp",
                "$XDG/agents/skills",
            ),
            (
                "cline",
                "Cline",
                ".agents/skills",
                None,
                "$HOME/.cline",
                "$HOME/.agents/skills",
            ),
            (
                "continue",
                "Continue",
                ".continue/skills",
                None,
                "$HOME/.continue",
                "$HOME/.continue/skills",
            ),
            (
                "cursor",
                "Cursor",
                ".agents/skills",
                None,
                "$HOME/.cursor",
                "$HOME/.cursor/skills",
            ),
            (
                "gemini-cli",
                "Gemini CLI",
                ".agents/skills",
                None,
                "$HOME/.gemini",
                "$HOME/.gemini/skills",
            ),
            (
                "github-copilot",
                "GitHub Copilot",
                ".agents/skills",
                None,
                "$HOME/.copilot",
                "$HOME/.copilot/skills",
            ),
            (
                "goose",
                "Goose",
                ".goose/skills",
                None,
                "$XDG/goose",
                "$XDG/goose/skills",
            ),
            (
                "kiro-cli",
                "Kiro CLI",
                ".kiro/skills",
                None,
                "$HOME/.kiro",
                "$HOME/.kiro/skills",
            ),
            (
                "opencode",
                "OpenCode",
                ".agents/skills",
                None,
                "$XDG/opencode",
                "$XDG/opencode/skills",
            ),
            (
                "qwen-code",
                "Qwen Code",
                ".qwen/skills",
                None,
                "$HOME/.qwen",
                "$HOME/.qwen/skills",
            ),
            (
                "roo",
                "Roo Code",
                ".roo/skills",
                None,
                "$HOME/.roo",
                "$HOME/.roo/skills",
            ),
            (
                "windsurf",
                "Windsurf",
                ".windsurf/skills",
                None,
                "$HOME/.codeium/windsurf",
                "$HOME/.codeium/windsurf/skills",
            ),
            (
                "zed",
                "Zed",
                ".agents/skills",
                None,
                "$XDG/zed",
                "$HOME/.agents/skills",
            ),
        ];

        let shown = |location: Location| {
            let base = match location.base {
                Base::Home => "$HOME",
                Base::XdgConfig => "$XDG",
            };
            format!("{base}/{}", location.path)
        };

        let actual: Vec<Row> = Agent::all()
            .map(|a| {
                (
                    a.flag(),
                    a.label(),
                    a.project_skills_dir(),
                    a.home_env(),
                    shown(a.spec().home),
                    shown(a.spec().global),
                )
            })
            .collect();

        let expected: Vec<Row> = expected
            .iter()
            .map(|(f, l, p, e, h, g)| (*f, *l, *p, *e, h.to_string(), g.to_string()))
            .collect();

        assert_eq!(actual, expected);
    }

    /// The properties the table has to hold whatever is in it, so a row added
    /// later cannot break them quietly.
    #[test]
    fn the_table_is_internally_consistent() {
        let mut flags: Vec<&str> = Agent::all().map(Agent::flag).collect();
        let count = flags.len();
        flags.sort_unstable();
        flags.dedup();
        assert_eq!(flags.len(), count, "two agents share an `--agent` value");

        for agent in Agent::all() {
            assert_eq!(
                Agent::from_flag(agent.flag()),
                Some(agent),
                "`{}` does not round-trip",
                agent.flag()
            );
            let spec = agent.spec();
            assert!(
                !spec.project_skills_dir.is_empty() && !spec.project_skills_dir.starts_with('/'),
                "{}'s project directory has to be relative: {}",
                agent.flag(),
                spec.project_skills_dir
            );
            for location in [spec.home, spec.global] {
                assert!(
                    !location.path.is_empty() && !location.path.starts_with('/'),
                    "{}'s {location:?} has to be relative to its base",
                    agent.flag()
                );
            }
        }

        assert_eq!(Agent::from_flag("no-such-agent"), None);
        // A spelling from the wider ecosystem that this table deliberately
        // does not carry — `--dir` is the answer for those.
        assert_eq!(Agent::from_flag("aider-desk"), None);
    }

    /// The two numbers the module docs quote, computed rather than restated.
    ///
    /// They were both wrong when first written — "six directories" for what is
    /// eight, and "four of the fifteen" diverge for what is three, because
    /// Windsurf's global really is its home plus `skills` like everyone
    /// else's. Prose about a table is prose that drifts from it; this fails
    /// when it does.
    #[test]
    fn the_counts_in_the_module_docs_are_the_tables_own() {
        let distinct: BTreeSet<&str> = Agent::all().map(Agent::project_skills_dir).collect();
        assert_eq!(
            distinct.len(),
            8,
            "the docs say eight distinct project directories: {distinct:?}"
        );

        let shared = Agent::all()
            .filter(|a| a.project_skills_dir() == ".agents/skills")
            .count();
        assert_eq!(
            shared, 8,
            "the docs say eight agents share `.agents/skills`"
        );

        // An agent "diverges" when its global directory is not simply its home
        // plus `skills` — which is what makes two `Location`s necessary.
        let diverging: Vec<&str> = Agent::all()
            .filter(|agent| {
                let spec = agent.spec();
                let same_base = spec.home.base == spec.global.base;
                let nested = spec.global.path == format!("{}/skills", spec.home.path);
                !(same_base && nested)
            })
            .map(Agent::flag)
            .collect();
        assert_eq!(
            diverging,
            ["amp", "cline", "zed"],
            "the docs name exactly these three as diverging"
        );
    }

    /// The convergence the module docs claim, asserted rather than described:
    /// most of these agents read one shared project directory, so asking for
    /// all of them writes far fewer than fifteen directories.
    #[test]
    fn the_shared_project_directory_collapses_most_of_the_roster() {
        let shared = Agent::all()
            .filter(|a| a.project_skills_dir() == ".agents/skills")
            .count();
        assert!(
            shared >= 8,
            "only {shared} agents read `.agents/skills`; the table has lost the convention"
        );

        let all: Vec<Agent> = Agent::all().collect();
        let out = resolve(None, &all, false, &AgentHomes::default(), true).unwrap();
        assert!(
            out.len() < all.len(),
            "asking for every agent produced one destination each: {out:?}"
        );
        assert!(
            out.iter()
                .any(|d| d.root.as_path() == Path::new(".agents/skills")),
            "the shared directory is not among {out:?}"
        );
    }

    /// Detection is the presence of the directory, and nothing else.
    #[test]
    fn an_agent_is_detected_when_its_home_directory_is_there() {
        let present = scratch("detected-present");
        let absent = present.join("not-created");

        let both = AgentHomes::describing(&[
            (agent("claude-code"), present.clone()),
            (agent("codex"), present.clone()),
        ]);
        assert_eq!(both.detected(), vec![agent("claude-code"), agent("codex")]);

        let one =
            AgentHomes::describing(&[(agent("claude-code"), present), (agent("codex"), absent)]);
        assert_eq!(one.detected(), vec![agent("claude-code")]);

        assert!(AgentHomes::default().detected().is_empty());
    }

    /// Saying nothing about the home is an error rather than a guess, and the
    /// error differs depending on whether there is a variable to suggest.
    #[test]
    fn a_home_that_cannot_be_found_is_an_error_naming_the_way_out() {
        let homes = AgentHomes::describing(&[(agent("claude-code"), PathBuf::from("/h/.claude"))]);
        assert_eq!(
            homes.global_skills_dir(agent("claude-code")).unwrap(),
            PathBuf::from("/h/.claude/skills")
        );

        let err = homes
            .global_skills_dir(agent("codex"))
            .expect_err("no home was given for Codex");
        assert!(err.to_string().contains("CODEX_HOME"), "{err}");

        // An agent with no relocation variable cannot be told to set one.
        let err = homes
            .global_skills_dir(agent("cursor"))
            .expect_err("no home was given for Cursor");
        let message = err.to_string();
        assert!(message.contains("--dir"), "{message}");
        assert!(!message.contains("Set "), "{message}");
    }

    /// `export CLAUDE_CONFIG_DIR=` is how a shell clears a variable, and
    /// reading it literally would resolve `skills` in the current directory.
    ///
    /// The one test here that has to touch the process environment, so it
    /// covers both variables in one body and puts them back.
    // `env::set_var` is unsafe because another thread may be reading the
    // environment. This test is the only reader inside itself, it restores
    // what it found, and there is no other way to exercise a variable the
    // real `from_env` reads. The crate denies `unsafe_code` so that saying
    // this out loud is the price of the exception.
    #[allow(unsafe_code)]
    #[test]
    fn an_empty_home_variable_is_not_a_home() {
        for agent in Agent::all() {
            let Some(variable) = agent.home_env() else {
                continue;
            };
            let previous = std::env::var_os(variable);
            // SAFETY: single-threaded within this test, and restored below.
            unsafe { std::env::set_var(variable, "") };
            let from_env = AgentHomes::from_env();
            let resolved = from_env.home(agent).map(Path::to_path_buf);
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(variable, value),
                    None => std::env::remove_var(variable),
                }
            }

            assert_eq!(
                resolved,
                resolve_location(agent.spec().home),
                "an empty {variable} was read as a home directory"
            );
        }
    }

    /// The variable moves the skills directory with the home, rather than
    /// only the place detection looks.
    // `env::set_var` is unsafe because another thread may be reading the
    // environment. This test is the only reader inside itself, it restores
    // what it found, and there is no other way to exercise a variable the
    // real `from_env` reads. The crate denies `unsafe_code` so that saying
    // this out loud is the price of the exception.
    #[allow(unsafe_code)]
    #[test]
    fn the_relocation_variable_moves_the_skills_directory_too() {
        let variable = "CODEX_HOME";
        let previous = std::env::var_os(variable);
        // SAFETY: single-threaded within this test, and restored below.
        unsafe { std::env::set_var(variable, "/somewhere/else") };
        let homes = AgentHomes::from_env();
        unsafe {
            match previous {
                Some(value) => std::env::set_var(variable, value),
                None => std::env::remove_var(variable),
            }
        }

        assert_eq!(
            homes.global_skills_dir(agent("codex")).unwrap(),
            PathBuf::from("/somewhere/else/skills")
        );
    }

    /// `$XDG_CONFIG_HOME` is honoured on every platform, and falls back to
    /// `~/.config` rather than to the OS config directory.
    // `env::set_var` is unsafe because another thread may be reading the
    // environment. This test is the only reader inside itself, it restores
    // what it found, and there is no other way to exercise a variable the
    // real `from_env` reads. The crate denies `unsafe_code` so that saying
    // this out loud is the price of the exception.
    #[allow(unsafe_code)]
    #[test]
    fn the_xdg_agents_follow_xdg_and_not_the_platform() {
        let variable = "XDG_CONFIG_HOME";
        let previous = std::env::var_os(variable);
        // SAFETY: single-threaded within this test, and restored below.
        unsafe { std::env::set_var(variable, "/xdg") };
        let homes = AgentHomes::from_env();
        unsafe {
            match previous {
                Some(value) => std::env::set_var(variable, value),
                None => std::env::remove_var(variable),
            }
        }

        assert_eq!(
            homes.global_skills_dir(agent("opencode")).unwrap(),
            PathBuf::from("/xdg/opencode/skills")
        );
        assert_eq!(
            homes.global_skills_dir(agent("amp")).unwrap(),
            PathBuf::from("/xdg/agents/skills")
        );
        // Zed is detected under the config home but writes under $HOME.
        assert_eq!(
            homes.global_skills_dir(agent("zed")).unwrap(),
            dirs::home_dir().unwrap().join(".agents/skills")
        );
    }

    /// `--dir` is the sole destination, whatever else the machine has.
    #[test]
    fn a_named_directory_is_the_only_destination() {
        let homes = AgentHomes::describing(&[(agent("claude-code"), PathBuf::from("/h/.claude"))]);
        let out = resolve(Some(Path::new("./out")), &[], false, &homes, true).unwrap();
        assert_eq!(
            out,
            vec![Destination {
                root: PathBuf::from("./out"),
                source: None,
            }]
        );
    }

    /// Two agents that read from the same directory are one write.
    #[test]
    fn the_same_directory_twice_is_one_destination() {
        let shared = PathBuf::from("/h/shared");
        let homes = AgentHomes::describing(&[
            (agent("claude-code"), shared.clone()),
            (agent("codex"), shared),
        ]);

        let out = resolve(
            None,
            &[agent("claude-code"), agent("codex")],
            true,
            &homes,
            true,
        )
        .unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].root, PathBuf::from("/h/shared/skills"));

        // Asking for the same agent twice is the same story.
        let repeated = resolve(
            None,
            &[agent("codex"), agent("codex")],
            false,
            &AgentHomes::default(),
            true,
        )
        .unwrap();
        assert_eq!(repeated.len(), 1, "{repeated:?}");

        // And so is asking for three agents that share a project directory.
        let converged = resolve(
            None,
            &[agent("codex"), agent("cursor"), agent("zed")],
            false,
            &AgentHomes::default(),
            true,
        )
        .unwrap();
        assert_eq!(converged.len(), 1, "{converged:?}");
        assert_eq!(converged[0].root, PathBuf::from(".agents/skills"));
    }

    /// Project-relative by default, home-relative under `--global`.
    #[test]
    fn global_moves_the_destination_from_the_project_to_the_home() {
        let homes = AgentHomes::describing(&[(agent("claude-code"), PathBuf::from("/h/.claude"))]);

        let project = resolve(None, &[agent("claude-code")], false, &homes, true).unwrap();
        assert_eq!(project[0].root, PathBuf::from(".claude/skills"));
        assert!(project[0].source.as_deref().unwrap().contains("project"));

        let global = resolve(None, &[agent("claude-code")], true, &homes, true).unwrap();
        assert_eq!(global[0].root, PathBuf::from("/h/.claude/skills"));
        assert!(global[0]
            .source
            .as_deref()
            .unwrap()
            .contains("all projects"));
    }

    /// Writing nothing, silently, is the one outcome a caller cannot act on.
    #[test]
    fn nothing_to_write_to_is_an_error_that_names_the_flags() {
        let nowhere = AgentHomes::default();
        let err = resolve(None, &[], false, &nowhere, true)
            .expect_err("no agent, no --dir, and nothing detected");
        let message = err.to_string();
        for flag in ["--agent", "--dir"] {
            assert!(message.contains(flag), "{message} does not mention {flag}");
        }
        for agent in Agent::all() {
            assert!(
                message.contains(agent.flag()),
                "{message} does not mention {}",
                agent.flag()
            );
        }

        // The same call is allowed to answer "nowhere" when the caller is only
        // asking, rather than about to write.
        assert!(resolve(None, &[], false, &nowhere, false)
            .unwrap()
            .is_empty());
    }
}
