//! en: Config file lookup (docs/cli.ja.md §3.3): `./ch32rv.toml`, then
//! `~/.config/ch32rv/config.toml`. Only probe aliases and defaults live here.
//! ja: 設定ファイル探索(docs/cli.ja.md §3.3)。probe 別名と既定値のみを持つ。

use std::path::PathBuf;

fn config_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("ch32rv.toml")];
    // en: TODO(M2): Windows/macOS config dirs; HOME covers Linux for now.
    // ja: TODO(M2): Windows/macOS の config dir 対応。当面は HOME(Linux)のみ。
    if let Ok(home) = std::env::var("HOME") {
        paths.push(
            PathBuf::from(home)
                .join(".config")
                .join("ch32rv")
                .join("config.toml"),
        );
    }
    paths
}

/// en: Resolve a `name:` probe alias to its selector string from `[probes]`.
/// ja: `name:` の probe 別名を `[probes]` から selector 文字列へ解決する。
pub fn probe_alias(name: &str) -> Option<String> {
    for path in config_paths() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let value: toml::Value = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning[config-parse]: {}: {e}", path.display());
                continue;
            }
        };
        if let Some(sel) = value
            .get("probes")
            .and_then(|t| t.get(name))
            .and_then(|v| v.as_str())
        {
            return Some(sel.to_owned());
        }
    }
    None
}
