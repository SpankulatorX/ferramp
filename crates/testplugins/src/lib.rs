//! Test plugins for Ferramp's plugin host, one DLL exporting every classic
//! entry point. Copy it to `in_ferramptest.dll`, `out_ferramptest.dll`,
//! `dsp_ferramptest.dll`, `vis_ferramptest.dll` and `gen_ferramptest.dll`;
//! the host picks the entry point from the file name, as the original
//! player did. Results go to `$FERRAMP_TEST_DIR` (a Windows path).
//!
//! - Input: `*.fttone` files (`freq=440 seconds=3 rate=44100 channels=2`)
//!   play a sine. The pull decoder refuses paths containing `noxr`, so both
//!   decoding paths get exercised.
//! - Output: appends the PCM it receives to `out.raw`.
//! - DSP: module 0 inverts the samples, module 1 doubles every frame.
//! - Visualization: logs renders with the sum of the spectrum to `vis.log`.
//! - General: queries the player window and logs what it learns to
//!   `gen.log`; `FERRAMP_TEST_GEN_CMD=next` makes it press Next once.

#![allow(clippy::missing_safety_doc)]

#[cfg(windows)]
mod imp;

#[cfg(windows)]
pub use imp::*;

/// Parses an `.fttone` description: whitespace or newline separated
/// `key=value` pairs.
pub fn parse_tone(text: &str) -> (f64, f64, u32, u32) {
    let (mut freq, mut secs, mut rate, mut ch) = (440.0, 3.0, 44100, 2);
    for kv in text.split_whitespace() {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        match k {
            "freq" => freq = v.parse().unwrap_or(freq),
            "seconds" => secs = v.parse().unwrap_or(secs),
            "rate" => rate = v.parse().unwrap_or(rate),
            "channels" => ch = v.parse().unwrap_or(ch),
            _ => {}
        }
    }
    (freq, secs, rate.clamp(8000, 192_000), ch.clamp(1, 2))
}

/// Sample `n` of the tone, 16-bit.
pub fn tone_sample(freq: f64, rate: u32, n: u64) -> i16 {
    let t = n as f64 / rate as f64;
    ((t * freq * std::f64::consts::TAU).sin() * 12000.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_parsing() {
        assert_eq!(
            parse_tone("freq=1000 seconds=2\nrate=48000 channels=1"),
            (1000.0, 2.0, 48000, 1)
        );
        assert_eq!(parse_tone("junk"), (440.0, 3.0, 44100, 2));
        assert_eq!(tone_sample(440.0, 44100, 0), 0);
    }
}
