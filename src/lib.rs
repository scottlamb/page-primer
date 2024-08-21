// Copyright (C) 2024 Scott Lamb <slamb@slamb.org>
// SPDX-License-Identifier: MIT OR Apache-2.0

#![doc = include_str!("../README.md")]

#[cfg(target_os = "linux")]
mod linux;
/// The options for priming.
///
/// By default, *nothing* will happen; call `mlock` and/or `remap` to change this.
#[derive(Default, Debug, PartialEq, Eq)]
#[must_use = "Options do nothing without Options::run"]
pub struct Options {
    mlock: bool,
    remap: bool,
}

impl Options {
    /// Sets whether `mlock` should be performed.
    #[inline]
    #[must_use]
    pub fn mlock(self, mlock: bool) -> Self {
        Self { mlock, ..self }
    }

    /// Sets whether pages should be remapped.
    #[inline]
    #[must_use]
    pub fn remap(self, remap: bool) -> Self {
        Self { remap, ..self }
    }

    /// Runs the selected operations.
    #[must_use]
    pub fn run(self) -> Output {
        #[cfg(target_os = "linux")]
        return linux::run(self);

        #[cfg(not(target_os = "linux"))]
        return Output { log: Vec::new() };
    }
}

#[must_use = "Output does nothing unless Output::log or Output::eprint is called"]
pub struct Output {
    log: Vec<(log::Level, String)>,
}

impl Output {
    /// Logs output using the [`log`] crate.
    pub fn log(&self) {
        for (level, msg) in &self.log {
            log::log!(*level, "{msg}");
        }
    }

    /// Prints output to stderr.
    pub fn eprint(&self) {
        for (_level, msg) in &self.log {
            eprintln!("{msg}");
        }
    }
}

/// Returns a builder for priming operations.
#[inline]
pub fn prime() -> Options {
    Options::default()
}
