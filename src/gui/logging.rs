//! GUI log bridge convenience wrapper.
//!
//! This module is a thin façade over [`lorag::logging::LogBridge`] so the GUI main
//! entrypoint does not need to know the broadcast-channel capacity / construction
//! details. The bridge itself is installed as a `tracing_subscriber::Layer` inside
//! [`lorag::logging::init_tracing`]; each tracing event (after the `EnvFilter`) is
//! formatted as a single ANSI-free line and broadcast to all subscribed receivers.
//!
//! The GUI logs page (G9) will hold a `Receiver<String>` obtained via
//! [`lorag::logging::LogBridge::subscribe`] and append incoming lines to its view.

use crate::logging::LogBridge;

/// Default broadcast channel capacity (most-recent lines kept for slow subscribers).
const DEFAULT_BRIDGE_CAPACITY: usize = 256;

/// Create a [`LogBridge`] with the given broadcast capacity and log the creation event.
///
/// Callers pass the returned bridge into [`lorag::logging::init_tracing`] as
/// `Some(bridge)`; the returned value is also retained by `AppState` (G4) so its
/// receiver side can be handed to the logs page.
///
/// Default capacity (256) matches plan §4 G3 and §8 risk table: it is enough to
/// absorb bursty startup logs while keeping memory bounded.
pub fn make_bridge(capacity: usize) -> LogBridge {
    let bridge = LogBridge::new(capacity);
    tracing::info!(capacity, "log bridge created");
    bridge
}

/// Create a bridge with the default capacity (see [`DEFAULT_BRIDGE_CAPACITY`]).
#[allow(dead_code)]
pub fn make_default_bridge() -> LogBridge {
    make_bridge(DEFAULT_BRIDGE_CAPACITY)
}
