//! en: In-repo tasks (`cargo xtask <task>`). Planned: `db-gen` (generate
//! `crates/target/generated/` from ch32-device-data / ch32-data; docs/architecture.ja.md §3).
//!
//! ja: repo 内タスク。予定: `db-gen`(ch32-device-data / ch32-data から生成)。

use std::process::ExitCode;

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("db-gen") => {
            eprintln!("xtask db-gen: not implemented yet (see docs/architecture.ja.md §3)");
            ExitCode::from(70)
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <task>\n\ntasks:\n  db-gen    generate crates/target/generated/ from pinned data repos"
            );
            ExitCode::from(2)
        }
    }
}
