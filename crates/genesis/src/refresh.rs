//! The 68000's DRAM refresh delay (#665).
//!
//! The console's refresh logic takes the 68000's bus away for a couple of clocks
//! at a steady rate. Measured on hardware (SpritesMind t=2323, "68K Refresh
//! Delays", Mask of Destiny 2016-01-30): running from the cartridge, "the delays
//! seem to alternate between being 126 cycles and 130 cycles apart" and "The
//! delays are only 2 cycles instead of 3 like it is with work RAM". The
//! reference emulator we measure against charges the round version of that:
//! "BlastEm adds 2 68K cycles of delay every 128 68K cycles unless refresh occurs
//! during a hardware access" (BlastEm trac ticket 24, "68K refresh delays are
//! only approximate"; `REFRESH_INTERVAL 128`, `REFRESH_DELAY 2` in its
//! `genesis.c`, counted in master clocks). This is that model:
//!
//! - every 128 68000 clocks of elapsed time owes 2 clocks of delay; the delay
//!   itself does not count towards the next interval;
//! - a refresh that falls inside a VDP port access costs nothing, since the
//!   68000 is already waiting on the VDP (`gen_update_refresh_free_access`):
//!   each such access is 4 68000 clocks that advance the counter without
//!   owing anything;
//! - a 68k -> VDP transfer restarts the count, and the frozen 68000 owes
//!   nothing while the transfer has the bus.
//!
//! Not modelled: the work-RAM refresh's separate timer and 3-clock delay from
//! the same hardware thread, which blastem does not model either.
//!
//! Without it the 68000 got about 1.5% more done per frame than blastem's, which
//! is what S&K's background decompressor measured as a steady lead (#529, #530,
//! #526).

use serde::{Deserialize, Serialize};

/// 68000 clocks between two refresh delays.
pub(crate) const INTERVAL_68K: u64 = 128;
/// 68000 clocks each refresh delays the 68000 by.
pub(crate) const DELAY_68K: u64 = 2;
/// 68000 clocks a VDP port access occupies the bus for, during which a refresh
/// is free (blastem: `4*MCLKS_PER_68K`).
const VDP_ACCESS_68K: u64 = 4;

/// The running refresh count, in master clocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refresh {
    /// Master clocks counted towards the next delay, always below one interval.
    counter_mclk: u64,
}

impl Refresh {
    /// Count `elapsed` master clocks of 68000 time, of which `vdp_accesses` VDP
    /// port accesses were free, and return the delay owed in master clocks.
    ///
    /// `mclk_per_68k` is the 68000's clock divider (7 on a Genesis).
    pub fn charge(&mut self, elapsed: u64, vdp_accesses: u32, mclk_per_68k: u64) -> u64 {
        let interval = INTERVAL_68K * mclk_per_68k;
        let free = (u64::from(vdp_accesses) * VDP_ACCESS_68K * mclk_per_68k).min(elapsed);
        self.counter_mclk += elapsed - free;
        let delay = DELAY_68K * mclk_per_68k * (self.counter_mclk / interval);
        // The free part moves the counter on, but a refresh it crosses is one
        // the 68000 was already stalled through.
        self.counter_mclk = (self.counter_mclk % interval + free) % interval;
        delay
    }

    /// A 68k -> VDP transfer has started: the count starts again from it.
    pub fn restart(&mut self) {
        self.counter_mclk = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MCLK: u64 = 7;

    #[test]
    fn nothing_is_owed_before_a_whole_interval() {
        let mut refresh = Refresh::default();
        assert_eq!(refresh.charge(127 * MCLK, 0, MCLK), 0);
    }

    #[test]
    fn every_128_clocks_owe_2() {
        let mut refresh = Refresh::default();
        assert_eq!(refresh.charge(127 * MCLK, 0, MCLK), 0);
        assert_eq!(refresh.charge(MCLK, 0, MCLK), 2 * MCLK);
        assert_eq!(refresh.charge(256 * MCLK, 0, MCLK), 4 * MCLK);
    }

    /// The remainder carries, so many small instructions owe what one long run
    /// of the same length owes.
    #[test]
    fn a_long_run_of_short_instructions_owes_2_per_128() {
        let mut refresh = Refresh::default();
        let owed: u64 = (0..12_800).map(|_| refresh.charge(4 * MCLK, 0, MCLK)).sum();
        assert_eq!(owed, 800 * MCLK, "51200 clocks owe 400 refreshes of 2");
    }

    #[test]
    fn a_refresh_inside_a_vdp_access_is_free() {
        let mut refresh = Refresh::default();
        // 122 clocks of work, then an 8-clock instruction that is one VDP
        // access: its own 4 clocks reach 126, and the boundary at 128 falls
        // inside the access.
        assert_eq!(refresh.charge(122 * MCLK, 0, MCLK), 0);
        assert_eq!(refresh.charge(8 * MCLK, 1, MCLK), 0);
        // ...and the count went on through it: 130 clocks in, 2 past the line,
        // so the next delay is 126 clocks away rather than 128.
        assert_eq!(refresh.charge(125 * MCLK, 0, MCLK), 0);
        assert_eq!(refresh.charge(MCLK, 0, MCLK), 2 * MCLK);
    }

    #[test]
    fn a_transfer_restarts_the_count() {
        let mut refresh = Refresh::default();
        assert_eq!(refresh.charge(120 * MCLK, 0, MCLK), 0);
        refresh.restart();
        assert_eq!(refresh.charge(120 * MCLK, 0, MCLK), 0);
        assert_eq!(refresh.charge(8 * MCLK, 0, MCLK), 2 * MCLK);
    }
}
