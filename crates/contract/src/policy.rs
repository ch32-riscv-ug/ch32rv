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
    /// Erase policy applied before programming.
    EraseMode {
        Auto => "auto",
        Sector => "sector",
        Chip => "chip",
        None => "none",
    }
}

policy_enum! {
    /// en: Verify policy after programming. Readback is the default; none is an explicit
    /// opt-out (docs/cli.ja.md §4.1).
    /// ja: 書き込み後の検証方針。readback が既定。none は明示選択。
    VerifyMode {
        Readback => "readback",
        Crc => "crc",
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
        Rtt => "rtt",
    }
}

policy_enum! {
    /// Recovery method (docs/cli.ja.md §4.1).
    RecoverMethod {
        PowerOff => "power-off",
        Nrst => "nrst",
        Unprotect => "unprotect",
        Unbrick => "unbrick",
    }
}

policy_enum! {
    /// Debug speed class (WCH-Link supports exactly three: 400 kHz / 4 MHz / 6 MHz).
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
