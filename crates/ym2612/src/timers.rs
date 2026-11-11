//! Timer A and timer B of the YM2612: the two counters whose overflows are
//! status bits 0 and 1.
//!
//! Both counters are modelled as counting up from the value their loader
//! register holds: timer A is 10 bits wide and overflows on the tick that would
//! take it to 1024, timer B is 8 bits wide and overflows on the tick that would
//! take it to 256, and each reloads from `&24`/`&25` or `&26` when it does
//! (YM2608 application manual "3. Register descriptions", `&24` to `&27`). A
//! timer only runs while its load bit of `&27` is set, and an overflow only
//! raises its status flag while its enable bit of `&27` is set.
//!
//! Both are stepped in output samples, which is #107's timing model: one
//! [`Timers::tick`] for timer A every sample and one for timer B every
//! [`TIMER_B_DIVISOR`] of them. The widths, the reload values and the flag rules
//! above come from the manual; the rates are the ticket's and none of them has
//! been measured against hardware. A hardware timer A tick is 64 sample periods
//! rather than the one modelled here, so absolute timer periods are 64x short,
//! and [#325](https://github.com/scottjab/rustsega/issues/325) is where the
//! measurements that fix that will land.

use crate::regs;

/// Timer A is a 10-bit counter: it overflows on the tick that would take it to
/// 1024 (YM2608 application manual "3. Register descriptions", `&24` and `&25`).
const TIMER_A_TOP: u16 = 1024;

/// Samples per timer B tick: timer B runs at 1/16 the rate of timer A (#107).
/// See the module note for how much of the timer rate is measured.
const TIMER_B_DIVISOR: u8 = 16;

/// The timer registers decoded, which is what the counter state machine reads.
///
/// Produced by [`regs::RegFile::timer_config`]; the loader values are the ones
/// the counters reload with, so a loader write between overflows changes the
/// length of the next period rather than the phase of the current one.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TimerConfig {
    /// Timer A loader, 10 bits: `&24` A0-A7, `&25` bits 1-0 A8-A9.
    pub(crate) a_value: u16,
    /// Timer B loader, 8 bits: `&26`.
    pub(crate) b_value: u8,
    /// `&27` bit 2: an overflow sets the status flag.
    pub(crate) a_enable: bool,
    /// `&27` bit 3: an overflow sets the status flag.
    pub(crate) b_enable: bool,
    /// `&27` bit 0: 0 stops timer A, 1 runs it.
    pub(crate) a_load: bool,
    /// `&27` bit 1: 0 stops timer B, 1 runs it.
    pub(crate) b_load: bool,
}

/// The two counters, their overflow flags and the prescaler between them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timers {
    /// Status bit 0, timer A overflow.
    flag_a: bool,
    /// Status bit 1, timer B overflow.
    flag_b: bool,
    /// Timer A's counter: raised every sample until it reaches
    /// [`TIMER_A_TOP`], and reloaded from [`TimerConfig::a_value`] when it does.
    counter_a: u16,
    /// Timer B's counter, raised every [`TIMER_B_DIVISOR`] samples.
    counter_b: u8,
    /// Samples since timer B's last tick, reset when timer B is started so the
    /// first period after a start is a whole one.
    divider: u8,
    /// The load bits as of the last timer write, so that the 0 to 1 edge of
    /// starting a timer can be seen.
    running_a: bool,
    running_b: bool,
}

impl Timers {
    /// Both timers stopped, their flags clear and both counters at 0.
    pub(crate) fn new() -> Self {
        Self {
            flag_a: false,
            flag_b: false,
            counter_a: 0,
            counter_b: 0,
            divider: 0,
            running_a: false,
            running_b: false,
        }
    }

    /// One of `&24` to `&27` has been written, with `value` and the register
    /// file already decoded into `config`.
    ///
    /// A 0 to 1 on a load bit starts that timer, and starts it from its loader
    /// value rather than from wherever its counter had got to: `&27` bits 0 and
    /// 1 are the LOAD/LOADB start-and-stop pair, and a timer that is started
    /// counts from its loader (YM2608 application manual `&27`). A write to a
    /// loader while the timer runs is not a restart - it only changes what the
    /// next overflow reloads with.
    ///
    /// INFERRED: neither #107 nor the YM2608 manual says what happens to a
    /// running counter when its loader is rewritten mid-period, and the
    /// difference is only visible to a driver that retargets a running timer.
    /// [#325](https://github.com/scottjab/rustsega/issues/325) is the place that
    /// will pin the timers down on hardware.
    pub(crate) fn register_written(&mut self, register: u8, value: u8, config: &TimerConfig) {
        if register == regs::REG_CONTROL {
            if value & regs::RESET_A_BIT != 0 {
                self.flag_a = false;
            }
            if value & regs::RESET_B_BIT != 0 {
                self.flag_b = false;
            }
        }

        if config.a_load && !self.running_a {
            self.counter_a = config.a_value;
        }
        if config.b_load && !self.running_b {
            self.counter_b = config.b_value;
            self.divider = 0;
        }
        self.running_a = config.a_load;
        self.running_b = config.b_load;
    }

    /// Advance both counters by one output sample: timer A every sample, timer B
    /// once every [`TIMER_B_DIVISOR`] of them.
    ///
    /// The divider runs whether or not timer B is running, and is reset when
    /// timer B is started, so a started timer B always gets a whole 16 samples
    /// before its first tick.
    pub(crate) fn tick(&mut self, config: &TimerConfig) {
        if self.running_a {
            self.tick_a(config);
        }

        self.divider += 1;
        if self.divider == TIMER_B_DIVISOR {
            self.divider = 0;
            if self.running_b {
                self.tick_b(config);
            }
        }
    }

    /// One timer A tick: count up, and on reaching 1024 reload from `&24`/`&25`
    /// and raise the status flag while `&27` bit 2, the timer A overflow enable,
    /// is set (YM2608 application manual `&24` to `&27`).
    fn tick_a(&mut self, config: &TimerConfig) {
        let next = self.counter_a + 1;
        if next >= TIMER_A_TOP {
            self.counter_a = config.a_value;
            if config.a_enable {
                self.flag_a = true;
            }
        } else {
            self.counter_a = next;
        }
    }

    /// One timer B tick, the 8-bit version of [`Timers::tick_a`].
    fn tick_b(&mut self, config: &TimerConfig) {
        match self.counter_b.checked_add(1) {
            Some(next) => self.counter_b = next,
            None => {
                self.counter_b = config.b_value;
                if config.b_enable {
                    self.flag_b = true;
                }
            }
        }
    }

    /// Whether timer A overflowed with its enable bit set since the flag was
    /// last reset: status bit 0.
    pub(crate) fn flag_a(&self) -> bool {
        self.flag_a
    }

    /// Whether timer B overflowed with its enable bit set since the flag was
    /// last reset: status bit 1.
    pub(crate) fn flag_b(&self) -> bool {
        self.flag_b
    }
}
