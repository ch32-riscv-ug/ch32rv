//! en: CLI progress sink: turns [`Event`]s into stderr output per --progress
//! (docs/cli.ja.md §3.5). `ndjson` writes one JSON event per line; `bar`/default prints
//! coarse phase lines; `none` is silent. Retries are always surfaced.
//! ja: CLI の進捗シンク。--progress に応じて [`Event`] を stderr へ出す。

use std::io::Write;

use ch32rv_contract::event::Event;
use ch32rv_contract::progress::ProgressSink;

use crate::args::{Cli, ProgressMode};

pub struct CliSink {
    mode: ProgressMode,
}

/// Pick the sink for this invocation. JSON result mode implies quiet phase output.
pub fn sink(cli: &Cli) -> CliSink {
    let mode = cli.progress.unwrap_or_else(|| {
        // Default: ndjson when --json (machine), otherwise bar on a tty, else none.
        if cli.json {
            ProgressMode::None
        } else if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            ProgressMode::Bar
        } else {
            ProgressMode::None
        }
    });
    CliSink { mode }
}

impl ProgressSink for CliSink {
    fn event(&self, ev: &Event) {
        match self.mode {
            ProgressMode::None => {
                // Even when silent, a retry is worth a line (docs/cli.ja.md §3.5).
                if let Event::Retry {
                    phase,
                    attempt,
                    cause,
                } = ev
                {
                    eprintln!("retry: {phase} attempt {attempt} ({cause})");
                }
            }
            ProgressMode::Ndjson => {
                if let Ok(line) = serde_json::to_string(ev) {
                    let mut err = std::io::stderr().lock();
                    let _ = writeln!(err, "{line}");
                }
            }
            ProgressMode::Bar => match ev {
                Event::Phase { name, .. } => eprintln!("{name}..."),
                Event::Retry {
                    phase,
                    attempt,
                    cause,
                } => {
                    eprintln!("retry: {phase} attempt {attempt} ({cause})")
                }
                Event::Warn { code, msg } => eprintln!("warning[{code}]: {msg}"),
                _ => {}
            },
        }
    }
}
