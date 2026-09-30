//! How a workflow run shows its steps on stderr.
//!
//! Two modes, chosen once per run. Where the line can be redrawn, each step
//! is its title, then what it reports, indented, then one status line: a
//! spinner while it runs, a mark and a duration once it ends. Only that last
//! line is ever redrawn, so a detail that wraps cannot throw the drawing off. Anywhere else
//! — a pipe, a CI log, an agent reading stderr — each step is one plain
//! `[n/total]` line and its reports follow as they arrive, with nothing that
//! only makes sense on a screen.
//!
//! A script reports through its stderr. A line starting with
//! [`PROGRESS_PREFIX`] replaces the text beside the spinner and is dropped
//! where there is none; any other line is a detail, kept in both modes.
//!
//! A command step is never animated. It keeps the terminal, where a
//! confirmation prompt has to be able to reach the person running it, so
//! nothing here may draw over its output.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::output::style;

/// Marks a script's stderr line as progress rather than a detail.
pub const PROGRESS_PREFIX: &str = "::progress ";

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);
const DETAIL_INDENT: &str = "    ";

pub struct Display {
    live: bool,
    color: bool,
    total: usize,
}

impl Display {
    pub fn for_stderr(total: usize) -> Self {
        let terminal = std::io::stderr().is_terminal();
        Display {
            live: style::redraws(terminal),
            color: style::enabled(terminal),
            total,
        }
    }

    /// A step that will not run, because this is a dry run and it did not
    /// promise to write nothing.
    pub fn skipped(&self, index: usize, title: &str, label: &str) {
        let line = if self.live {
            format!(
                "{} {}  {}",
                style::paint("○", style::DIM, self.color),
                style::paint(title, style::DIM, self.color),
                style::paint("skipped in a dry run", style::DIM, self.color),
            )
        } else {
            format!(
                "{} {} {}",
                self.counter(index),
                style::paint(title, style::BOLD, self.color),
                style::paint(
                    &format!("({label}, skipped in a dry run)"),
                    style::DIM,
                    self.color
                ),
            )
        };
        eprintln!("{line}");
    }

    pub fn start(&self, index: usize, title: &str, label: &str, animate: bool) -> StepView {
        let view = StepView {
            state: Arc::new(Mutex::new(State {
                progress: String::new(),
                frame: 0,
                spinning: self.live && animate,
            })),
            stop: Arc::new(AtomicBool::new(false)),
            ticker: None,
            started: Instant::now(),
            live: self.live,
            color: self.color,
        };

        if self.live {
            eprintln!("{}", self.header(title, (!animate).then_some(label)));
        }
        if self.live && animate {
            draw_spinner(&view.state.lock().expect("progress state"), self.color);
            let state = Arc::clone(&view.state);
            let stop = Arc::clone(&view.stop);
            let color = self.color;
            let ticker = std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(TICK);
                    let mut state = state.lock().expect("progress state");
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    state.frame = (state.frame + 1) % FRAMES.len();
                    draw_spinner(&state, color);
                }
            });
            return StepView {
                ticker: Some(ticker),
                ..view
            };
        }

        if !self.live {
            eprintln!(
                "{} {} {}",
                self.counter(index),
                style::paint(title, style::BOLD, self.color),
                style::paint(&format!("({label})"), style::DIM, self.color),
            );
        }
        view
    }

    /// `label` only for a step without a spinner, which has nowhere else to
    /// say what is running.
    fn header(&self, title: &str, label: Option<&str>) -> String {
        let mut line = format!(
            "{} {}",
            style::paint("▸", style::ACCENT, self.color),
            style::paint(title, style::BOLD, self.color),
        );
        if let Some(label) = label {
            line.push_str(&format!(
                "  {}",
                style::paint(label, style::DIM, self.color)
            ));
        }
        line
    }

    fn counter(&self, index: usize) -> String {
        style::paint(
            &format!("[{}/{}]", index + 1, self.total),
            style::DIM,
            self.color,
        )
    }
}

struct State {
    progress: String,
    frame: usize,
    spinning: bool,
}

/// One running step's place on stderr. Shared with the thread reading the
/// step's stderr, so every write goes through the one lock.
pub struct StepView {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
    started: Instant,
    live: bool,
    color: bool,
}

impl StepView {
    /// A handle the stderr reader can own.
    pub fn reporter(&self) -> Reporter {
        Reporter {
            state: Arc::clone(&self.state),
            live: self.live,
            color: self.color,
        }
    }

    /// Replaces the status line with how the step ended. Nothing in the plain
    /// mode: the next step's line, or the error, already says so.
    pub fn finish(mut self, succeeded: bool) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        if !self.live {
            return;
        }
        let state = self.state.lock().expect("progress state");
        if state.spinning {
            clear_line();
        }
        let took = elapsed(self.started.elapsed());
        let (mark, paint, text) = if succeeded {
            ("✓", style::GREEN, took)
        } else {
            ("✗", style::RED, format!("failed after {took}"))
        };
        // A blank line after, so each step reads as its own block.
        eprintln!(
            "  {} {}\n",
            style::paint(mark, paint, self.color),
            style::paint(&text, style::DIM, self.color),
        );
        let _ = std::io::stderr().flush();
    }
}

/// What the thread reading a step's stderr holds: the same view, without the
/// right to finish it.
pub struct Reporter {
    state: Arc<Mutex<State>>,
    live: bool,
    color: bool,
}

impl Reporter {
    /// One line the step wrote to stderr.
    pub fn report(&self, line: &str) {
        let mut state = self.state.lock().expect("progress state");
        match line.strip_prefix(PROGRESS_PREFIX) {
            Some(progress) => {
                if state.spinning {
                    state.progress = progress.trim().to_string();
                    draw_spinner(&state, self.color);
                }
            }
            None if state.spinning => {
                clear_line();
                eprintln!("{DETAIL_INDENT}{line}");
                draw_spinner(&state, self.color);
            }
            None if self.live => eprintln!("{DETAIL_INDENT}{line}"),
            None => eprintln!("{line}"),
        }
    }
}

fn draw_spinner(state: &State, color: bool) {
    let progress = if state.progress.is_empty() {
        "working"
    } else {
        &state.progress
    };
    eprint!(
        "\r\x1b[2K  {} {}",
        style::paint(FRAMES[state.frame], style::ACCENT, color),
        style::paint(progress, style::DIM, color),
    );
    let _ = std::io::stderr().flush();
}

fn clear_line() {
    eprint!("\r\x1b[2K");
}

fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let whole = duration.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_reads_in_seconds_then_minutes() {
        assert_eq!(elapsed(Duration::from_millis(2840)), "2.8s");
        assert_eq!(elapsed(Duration::from_secs(72)), "1m 12s");
    }
}
