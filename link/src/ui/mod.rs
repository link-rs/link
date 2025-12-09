//! UI (User Interface) chip - handles buttons and user interaction.

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, Led, MgmtToUi, NetToUi, RawMutex, Sender, Tlv, UiToMgmt,
    UiToNet, WriteTlv,
};
use embedded_hal::digital::StatefulOutputPin;
use embedded_hal_async::digital::Wait;
use embedded_io_async::{Read, Write};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Button {
    A,
    B,
}

enum Event {
    Mgmt(Tlv<MgmtToUi>),
    Net(Tlv<NetToUi>),
    ButtonDown(Button),
    ButtonUp(Button),
}

pub struct App<W, R, LR, LG, LB, BA, BB> {
    to_mgmt: W,
    to_net: W,
    from_mgmt: R,
    from_net: R,
    led: (LR, LG, LB),
    button_a: BA,
    button_b: BB,
}

impl<W, R, LR, LG, LB, BA, BB> App<W, R, LR, LG, LB, BA, BB>
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
    BA: Wait,
    BB: Wait,
{
    pub fn new(
        to_mgmt: W,
        from_mgmt: R,
        to_net: W,
        from_net: R,
        led: (LR, LG, LB),
        button_a: BA,
        button_b: BB,
    ) -> Self {
        Self {
            to_mgmt,
            to_net,
            from_mgmt,
            from_net,
            led,
            button_a,
            button_b,
        }
    }

    #[allow(unreachable_code)]
    pub async fn run(self) -> ! {
        info!("ui: starting");

        let Self {
            mut to_mgmt,
            mut to_net,
            from_mgmt,
            from_net,
            led,
            button_a,
            button_b,
        } = self;

        // Initialize LED
        let mut led = Led::new(led.0, led.1, led.2);
        led.set(Color::Blue);

        const MAX_QUEUE_DEPTH: usize = 4;
        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mgmt_read_task = read_tlv_loop(from_mgmt, channel.sender(), Event::Mgmt);
        let net_read_task = read_tlv_loop(from_net, channel.sender(), Event::Net);
        let button_a_task = button_monitor(button_a, Button::A, channel.sender());
        let button_b_task = button_monitor(button_b, Button::B, channel.sender());

        let handle_task = async {
            info!("ui: ready to handle events");
            loop {
                match channel.receive().await {
                    Event::Mgmt(tlv) => handle_mgmt(tlv, &mut to_mgmt, &mut to_net).await,
                    Event::Net(tlv) => handle_net(tlv, &mut to_mgmt).await,
                    Event::ButtonDown(button) => {
                        info!("ui: button {:?} down", button);
                    }
                    Event::ButtonUp(button) => {
                        info!("ui: button {:?} up", button);
                    }
                }
            }
        };

        futures::join!(
            mgmt_read_task,
            net_read_task,
            button_a_task,
            button_b_task,
            handle_task
        );
        unreachable!()
    }
}

async fn button_monitor<'a, B: Wait, const N: usize>(
    mut button: B,
    which: Button,
    sender: Sender<'a, RawMutex, Event, N>,
) -> ! {
    loop {
        // Wait for button press (rising edge - active high with pull-down)
        let _ = button.wait_for_rising_edge().await;
        sender.send(Event::ButtonDown(which)).await;

        // Wait for button release (falling edge)
        let _ = button.wait_for_falling_edge().await;
        sender.send(Event::ButtonUp(which)).await;
    }
}

async fn handle_mgmt<M, N>(tlv: Tlv<MgmtToUi>, to_mgmt: &mut M, to_net: &mut N)
where
    M: WriteTlv<UiToMgmt>,
    N: WriteTlv<UiToNet>,
{
    match tlv.tlv_type {
        MgmtToUi::Ping => {
            info!("ui: mgmt ping, sending pong");
            to_mgmt.must_write_tlv(UiToMgmt::Pong, &tlv.value).await;
        }
        MgmtToUi::CircularPing => {
            info!("ui: mgmt circular ping -> net");
            to_net
                .must_write_tlv(UiToNet::CircularPing, &tlv.value)
                .await;
        }
    }
}

async fn handle_net<M>(tlv: Tlv<NetToUi>, to_mgmt: &mut M)
where
    M: WriteTlv<UiToMgmt>,
{
    match tlv.tlv_type {
        NetToUi::CircularPing => {
            info!("ui: net circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(UiToMgmt::CircularPing, &tlv.value)
                .await;
        }
    }
}
