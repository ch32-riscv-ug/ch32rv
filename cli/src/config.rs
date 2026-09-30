//! en: Config file lookup (docs/cli.ja.md §3.3): `./ch32rv.toml`, then the OS's config dir
//! (Linux `$XDG_CONFIG_HOME` or `~/.config`, macOS `~/Library/Application Support`, Windows
//! `%APPDATA%`) + `ch32rv/config.toml`. Only probe aliases and defaults live here.
//! ja: 設定ファイル探索。`./ch32rv.toml`、次に OS の設定の場所 + `ch32rv/config.toml`。probe 別名と既定値のみ。

use std::path::PathBuf;

fn config_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("ch32rv.toml")];
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    };
    if let Some(d) = dir {
        paths.push(d.join("ch32rv").join("config.toml"));
    }
    paths
}

/// The first config file's parsed table (a file that does not parse is warned about and skipped).
fn tables() -> impl Iterator<Item = (PathBuf, toml::Value)> {
    config_paths().into_iter().filter_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        match toml::from_str(&text) {
            Ok(v) => Some((path, v)),
            Err(e) => {
                eprintln!("warning[config-parse]: {}: {e}", path.display());
                None
            }
        }
    })
}

/// en: `[defaults] chip`: the `--chip` used when neither `--chip` nor `CH32RV_CHIP` gives one.
/// ja: `[defaults] chip`。`--chip` も `CH32RV_CHIP` も無いときの `--chip`。
pub fn default_chip() -> Option<String> {
    tables().find_map(|(_, v)| v.get("defaults")?.get("chip")?.as_str().map(str::to_owned))
}

/// en: Resolve a `name:` probe alias to its selector string from `[probes]`.
/// ja: `name:` の probe 別名を `[probes]` から selector 文字列へ解決する。
pub fn probe_alias(name: &str) -> Option<String> {
    tables().find_map(|(_, v)| v.get("probes")?.get(name)?.as_str().map(str::to_owned))
}
