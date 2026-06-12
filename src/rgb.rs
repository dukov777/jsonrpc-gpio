//! Minimal WS2812 (addressable RGB) driver for the on-board LED on GPIO48.
//!
//! Device-only (`#[cfg(target_os = "espidf")]`). Drives a single WS2812-class
//! pixel over the RMT peripheral — the LED needs a precise ~800 kHz one-wire
//! bit stream, which a plain `gpio_set_level` cannot produce.
//!
//! This is a small demo, not part of the JSON-RPC surface: `main` lights the
//! pixel green once at boot to prove the RMT path works end-to-end.
//!
//! Encoding: 24 bits, **GRB** order, MSB first. With `clock_divider = 1` the
//! RMT source clock is 80 MHz → 12.5 ns per tick, so the WS2812 bit timings are:
//! T0H 0.35 µs = 28 ticks, T0L 0.8 µs = 64, T1H 0.7 µs = 56, T1L 0.6 µs = 48.

// The legacy RMT API (TxRmtDriver/FixedLengthSignal) is the simplest fit for a
// one-pixel WS2812 demo; silence its deprecation notice for the whole module.
#![allow(deprecated)]

use esp_idf_hal::gpio::OutputPin;
use esp_idf_hal::rmt::config::TransmitConfig;
use esp_idf_hal::rmt::{FixedLengthSignal, PinState, Pulse, PulseTicks, RmtChannel, TxRmtDriver};
use esp_idf_hal::sys::EspError;

// WS2812 bit timings in RMT ticks (12.5 ns each at clock_divider = 1).
const T0H: u16 = 28;
const T0L: u16 = 64;
const T1H: u16 = 56;
const T1L: u16 = 48;

/// Drives a single on-board WS2812 pixel over RMT.
pub struct Ws2812<'d> {
    tx: TxRmtDriver<'d>,
}

impl<'d> Ws2812<'d> {
    /// Install the RMT TX driver on `pin` (GPIO48) using `channel`
    /// (`peripherals.rmt.channel0`).
    pub fn new<C: RmtChannel + 'd>(channel: C, pin: impl OutputPin + 'd) -> Result<Self, EspError> {
        let config = TransmitConfig::new().clock_divider(1);
        let tx = TxRmtDriver::new(channel, pin, &config)?;
        Ok(Self { tx })
    }

    /// Set the pixel color. Blocks until the bit stream has been sent.
    pub fn set_rgb(&mut self, r: u8, g: u8, b: u8) -> Result<(), EspError> {
        // WS2812 wire order is GRB, most-significant bit first.
        let grb = (u32::from(g) << 16) | (u32::from(r) << 8) | u32::from(b);

        let t0h = Pulse::new(PinState::High, PulseTicks::new(T0H)?);
        let t0l = Pulse::new(PinState::Low, PulseTicks::new(T0L)?);
        let t1h = Pulse::new(PinState::High, PulseTicks::new(T1H)?);
        let t1l = Pulse::new(PinState::Low, PulseTicks::new(T1L)?);

        let mut signal = FixedLengthSignal::<24>::new();
        for i in 0..24 {
            let bit = (grb >> (23 - i)) & 1;
            let pair = if bit == 1 { (t1h, t1l) } else { (t0h, t0l) };
            signal.set(i, &pair)?;
        }
        self.tx.start_blocking(&signal)
    }
}
