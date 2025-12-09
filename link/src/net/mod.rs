//! NET (Network) chip - handles network communication.

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, Led, MgmtToNet, NetToMgmt, NetToUi, RawMutex, Tlv, UiToNet,
    WriteTlv,
};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};

enum Event {
    Mgmt(Tlv<MgmtToNet>),
    Ui(Tlv<UiToNet>),
}

pub struct App<W, R, LR, LG, LB> {
    to_mgmt: W,
    to_ui: W,
    from_mgmt: R,
    from_ui: R,
    led: (LR, LG, LB),
}

impl<W, R, LR, LG, LB> App<W, R, LR, LG, LB>
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
{
    pub fn new(to_mgmt: W, from_mgmt: R, to_ui: W, from_ui: R, led: (LR, LG, LB)) -> Self {
        Self {
            to_mgmt,
            to_ui,
            from_mgmt,
            from_ui,
            led,
        }
    }

    #[allow(unreachable_code)]
    pub async fn run(self) -> ! {
        info!("net: starting");

        let Self {
            mut to_mgmt,
            mut to_ui,
            from_mgmt,
            from_ui,
            led,
        } = self;

        // Initialize LED
        let mut led = Led::new(led.0, led.1, led.2);
        led.set(Color::Yellow);

        const MAX_QUEUE_DEPTH: usize = 2;
        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mgmt_read_task = read_tlv_loop(from_mgmt, channel.sender(), Event::Mgmt);
        let ui_read_task = read_tlv_loop(from_ui, channel.sender(), Event::Ui);

        let handle_task = async {
            info!("net: ready to handle events");
            loop {
                match channel.receive().await {
                    Event::Mgmt(tlv) => handle_mgmt(tlv, &mut to_mgmt, &mut to_ui).await,
                    Event::Ui(tlv) => handle_ui(tlv, &mut to_mgmt).await,
                }
            }
        };

        futures::join!(mgmt_read_task, ui_read_task, handle_task);
        unreachable!()
    }
}

async fn handle_mgmt<M, U>(tlv: Tlv<MgmtToNet>, to_mgmt: &mut M, to_ui: &mut U)
where
    M: WriteTlv<NetToMgmt>,
    U: WriteTlv<NetToUi>,
{
    match tlv.tlv_type {
        MgmtToNet::Ping => {
            info!("net: mgmt ping, sending pong");
            to_mgmt.must_write_tlv(NetToMgmt::Pong, &tlv.value).await;
        }
        MgmtToNet::CircularPing => {
            info!("net: mgmt circular ping -> ui");
            to_ui
                .must_write_tlv(NetToUi::CircularPing, &tlv.value)
                .await;
        }
    }
}

async fn handle_ui<M>(tlv: Tlv<UiToNet>, to_mgmt: &mut M)
where
    M: WriteTlv<NetToMgmt>,
{
    match tlv.tlv_type {
        UiToNet::CircularPing => {
            info!("net: ui circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(NetToMgmt::CircularPing, &tlv.value)
                .await;
        }
    }
}
