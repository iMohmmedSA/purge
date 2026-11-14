//! Decides what to clean in each project and carries it out.
//!
//! [`decide()`] is the policy as a pure function; [`Engine`] feeds it real
//! projects and applies the resulting [`Action`]s through each ecosystem.

mod decide;
mod report;
mod run;
mod unit;

pub use decide::{Action, Reason, decide, stale_parts};
pub use report::{
    ActionOutcome, Outcome, RunReport, UnitReport, UnitStatus, format_age, format_bytes,
};
pub use run::{Engine, RunOptions};
pub use unit::Unit;
