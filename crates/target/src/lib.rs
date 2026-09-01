//! en: Target device DB. Never hand-written: the DB is generated from `ch32-device-data`
//! (geometry / memory map / option splits / DM addresses) and `ch32-data` (device_id), and
//! committed under `generated/` (docs/architecture.ja.md §3). Missing data is requested from
//! ch32-device-data (docs/data-requests/); until delivery, a provisional overlay under
//! `provisional/` fills the gap. Every SKU carries `verified` / `provisional` flags that
//! surface in tool output.
//! Currently only skeleton types; the generation pipeline (`xtask db-gen`) is not implemented.
//!
//! ja: target device DB。手書きせず、ch32-device-data と ch32-data からの生成物を
//! `generated/` に commit する。不足データは ch32-device-data へ依頼し、納品までは
//! `provisional/` の暫定 overlay で進む。SKU ごとに verified / provisional flag を持ち出力に出す。
//! 現状は型の骨組みのみ。

use serde::{Deserialize, Serialize};

/// en: The device DB: a read-only view over the generated data plus overlays.
/// ja: device DB。生成物 + overlay をまとめた読み取り専用ビュー。
#[derive(Debug, Default)]
pub struct Db {
    skus: Vec<SkuRecord>,
}

impl Db {
    /// en: Built-in (generated) DB. Empty until the generation pipeline lands.
    /// ja: 内蔵(生成済み)DB。生成 pipeline 実装までは空。
    pub fn builtin() -> Self {
        Self::default()
    }

    pub fn skus(&self) -> &[SkuRecord] {
        &self.skus
    }
}

/// One SKU record. Column meanings: docs/architecture.ja.md §3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkuRecord {
    pub sku: String,
    pub family: String,
    /// device_id read from memory (from measured evidence).
    pub device_id: Option<u32>,
    pub flash_bytes: u64,
    pub sram_bytes: u64,
    /// en: Verified on real silicon (kept distinct from merely "implemented").
    /// ja: 実機確認済みか(「実装済み」と区別する)。
    pub verified: bool,
    /// en: Comes from the provisional overlay (request to ch32-device-data pending).
    /// ja: 暫定 overlay 由来か(ch32-device-data へ依頼中)。
    pub provisional: bool,
}
