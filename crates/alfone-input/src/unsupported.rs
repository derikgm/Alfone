//! Placeholder for macOS and other platforms while their backends are
//! implemented (Windows now has a native backend in [`crate::windows`]).
//!
//! The traits and the protocol are already platform neutral; next:
//! - macOS: `CGEventTap` + `CGEventPost` (Accessibility permission).
//!
//! Returning a clear error is better than silently doing nothing.

use crate::{Capture, Inject};

pub fn create_capture() -> anyhow::Result<Box<dyn Capture>> {
    anyhow::bail!(
        "this platform's input backend is not implemented yet; \
         see docs/PLAN.md (M1). Linux/X11 is the reference backend."
    )
}

pub fn create_inject() -> anyhow::Result<Box<dyn Inject>> {
    anyhow::bail!(
        "this platform's input backend is not implemented yet; \
         see docs/PLAN.md (M1). Linux/X11 is the reference backend."
    )
}
