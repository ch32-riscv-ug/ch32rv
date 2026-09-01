//! en: Programming layer: orchestrates erase / program / verify / confirm-run
//! (docs/cli.ja.md §4.1). Flash stubs are built from in-repo sources (no prebuilt blobs) and
//! their hashes surface in `version --json` (docs/architecture.ja.md §3). For input images,
//! ELF uses `object`; Intel HEX gets a small in-house parser (the ihex crate has been stale
//! since 2020; docs/architecture.ja.md §1.3).
//! Currently only the policy-bundle skeleton.
//!
//! ja: 書き込み層。erase / program / verify / confirm-run の編成。flash stub は in-repo の
//! source から build して hash を `version --json` に出す。ELF は object、HEX は自前 parser
//! (ihex crate は停滞のため不採用)。現状は policy を束ねる型の骨組みのみ。

use ch32rv_contract::policy::{ConfirmRunMode, EraseMode, Region, ResetPolicy, VerifyMode};

/// en: Policy set for one `flash` invocation. Defaults match docs/cli.ja.md §4.1.
/// ja: `flash` 1 回分の方針。既定値は docs/cli.ja.md §4.1 と一致させる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashOptions {
    pub region: Region,
    pub erase: EraseMode,
    pub verify: VerifyMode,
    pub reset: ResetPolicy,
    pub confirm_run: Option<ConfirmRunMode>,
}

impl Default for FlashOptions {
    fn default() -> Self {
        Self {
            region: Region::Code,
            erase: EraseMode::Auto,
            verify: VerifyMode::Readback,
            reset: ResetPolicy::Run,
            confirm_run: None,
        }
    }
}
