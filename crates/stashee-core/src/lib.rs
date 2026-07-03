//! UI-agnostic kernel of stashee-terminal: workflows, tiling math, the
//! tmux/ssh command model, and persisted state.
//!
//! This crate must never depend on GTK or any UI toolkit — it is the
//! kernel every frontend reuses (see "Cross-platform strategy" in
//! docs/ARCHITECTURE.md). It also does no path discovery: the frontend
//! passes config and state paths in.

pub mod config;
pub mod layout;
pub mod model;
pub mod osc52;
pub mod ssh;
pub mod state;
pub mod tmux;
