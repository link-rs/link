//! MGMT chip command handlers.

use crate::{App, MgmtAction};

pub async fn handle_mgmt(
    action: MgmtAction,
    app: &mut App,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match action {
        MgmtAction::Ping { data } => {
            println!("Sending MGMT ping with data: {}", data);
            app.mgmt_ping(data.as_bytes()).await;
            Ok(Some("Received pong!".to_string()))
        }
        MgmtAction::Info => {
            Err("mgmt info requires bootloader mode - run as: ctl mgmt info".into())
        }
        MgmtAction::Flash { .. } => {
            Err("mgmt flash requires bootloader mode - run as: ctl mgmt flash <file>".into())
        }
    }
}
