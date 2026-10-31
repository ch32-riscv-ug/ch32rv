//! en: Operation-policy vocabulary (docs/cli.ja.md §4.1 and friends).
//! CLI value enums and JSON output share the same spelling (kebab-case / lowercase).
//! With the `clap` feature these double as clap ValueEnums.
//!
//! ja: 操作 policy の語彙(docs/cli.ja.md §4.1 ほか)。CLI の value enum と JSON 出力で
//! 同じ綴り(kebab-case / 小文字)を使う。`clap` feature でそのまま ValueEnum になる。

use serde::{Deserialize, Serialize};

macro_rules! policy_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
        #[serde(rename_all = "kebab-case")]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $s),+
                }
            }
        }
    };
}

policy_enum! {
    /// en: Erase policy applied before programming (docs/cli.ja.md §4.1). `keep-store` (the
    /// default) keeps the image's STORE region (ELF `.ch32rv_store`) and lets each path pick its
    /// fastest way; with no STORE it is `sector`.
    /// ja: 書き込み前の消去方針。`keep-store`(既定)は image の STORE を残し、経路ごとに速い方法を
    /// 選ぶ。STORE が無ければ `sector` と同じ。
    EraseMode {
        /// Erase the whole code flash first.
        Chip => "chip",
        /// Erase only the pages / sectors the image covers; flash outside the image is kept.
        Sector => "sector",
        /// Keep the STORE region; the path picks the fastest way (no STORE: `sector`).
        KeepStore => "keep-store",
        /// Do not erase (refused on paths that always erase what they write).
        None => "none",
    }
}

policy_enum! {
    /// en: Verify policy after programming. Readback is the default; none is an explicit
    /// opt-out (docs/cli.ja.md §4.1). A CRC on the target is added when one is implemented.
    /// ja: 書き込み後の検証方針。readback が既定。none は明示選択。target の上の CRC は実装したときに足す。
    VerifyMode {
        Readback => "readback",
        None => "none",
    }
}

policy_enum! {
    /// Reset policy after programming.
    ResetPolicy {
        Run => "run",
        Halt => "halt",
        None => "none",
    }
}

policy_enum! {
    /// How `--confirm-run` verifies that the target is running.
    ConfirmRunMode {
        /// Check the DM running state only.
        Status => "status",
        /// Additionally sample the PC (momentary halt, read dpc, resume) and check it lies in flash.
        Pc => "pc",
    }
}

policy_enum! {
    /// Input image format.
    ImageFormat {
        Auto => "auto",
        Elf => "elf",
        Hex => "hex",
        Bin => "bin",
        Uf2 => "uf2",
    }
}

policy_enum! {
    /// en: Monitor transport. Treated as distinct transports even when they appear on the
    /// same COM port (docs/cli.ja.md §4.5).
    /// ja: monitor の transport。同じ COM に見えても別物として扱う。
    MonitorSource {
        Uart => "uart",
        Sdi => "sdi",
        Dmdata => "dmdata",
        Dmseq => "dmseq",
        Rtt => "rtt",
        /// An OEP probe's fixture UART (`oep.fixture.uart`), at the monitor's baud rate.
        FixtureUart => "fixture-uart",
    }
}

policy_enum! {
    /// Recovery method (docs/cli.ja.md §4.1).
    RecoverMethod {
        PowerOff => "power-off",
        Nrst => "nrst",
        Unprotect => "unprotect",
        Unbrick => "unbrick",
        Auto => "auto",
    }
}

policy_enum! {
    /// Debug speed class. WCH-Link takes exactly three; their nominal 400 kHz / 4 MHz / 6 MHz are
    /// not what the wire runs at (WCH-LinkE fw 2.22 + CH32L103: low ~0.47 MHz, medium ~0.89 MHz,
    /// high ~0.89 MHz except on the flash program path, ~2.5 MHz on L103 / ~10 MHz on V203; wch-protocols protocols/link-to-target.ja.md#speed).
    SpeedClass {
        Low => "low",
        Medium => "medium",
        High => "high",
    }
}

policy_enum! {
    /// Named memory regions (docs/cli.ja.md §4.1).
    Region {
        Code => "code",
        System => "system",
        Option => "option",
        Eeprom => "eeprom",
        Ram => "ram",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn kebab_case_spelling() {
        assert_eq!(
            serde_json::to_string(&RecoverMethod::PowerOff).unwrap(),
            r#""power-off""#
        );
        assert_eq!(RecoverMethod::PowerOff.as_str(), "power-off");
        assert_eq!(VerifyMode::Readback.as_str(), "readback");
    }
}
