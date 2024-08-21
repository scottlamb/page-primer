// Copyright (C) 2024 Scott Lamb <slamb@slamb.org>
// SPDX-License-Identifier: MIT OR Apache-2.0

#![doc = include_str!("../README.md")]

#[cfg(target_os = "linux")]
mod linux;
/// The options for priming.
///
/// By default, *nothing* will happen; call `mlock` and/or `remap` to change this.
#[derive(Default, Debug, PartialEq, Eq)]
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
    pub fn run(self) {
        #[cfg(target_os = "linux")]
        linux::run(self)
    }
}

/// Returns a builder for priming operations.
#[inline]
#[must_use]
pub fn prime() -> Options {
    Options::default()
}
