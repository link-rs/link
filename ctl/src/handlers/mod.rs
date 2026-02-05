//! Command handlers for MGMT, UI, and NET chips.

mod mgmt;
mod net;
mod ui;

pub use mgmt::handle_mgmt;
pub use net::handle_net;
pub use ui::handle_ui;

/// Re-export Core type for handlers.
pub type Core = crate::Core;
