//! MGMT (Management) chip - coordinates communication between all chips.

use crate::info;
use crate::shared::{
    read_raw_loop, read_tlv_loop, Channel, Color, CtlToMgmt, Led, MgmtToCtl, MgmtToNet, MgmtToUi,
    RawMutex, Tlv, Value, WriteTlv, SYNC_WORD,
};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};

enum Event {
    Ctl(Tlv<CtlToMgmt>),
    Ui(Value),
    Net(Value),
}

pub struct App<W, R, RA, GA, BA, RB, GB, BB> {
    to_ctl: W,
    to_ui: W,
    to_net: W,
    from_ctl: R,
    from_ui: R,
    from_net: R,
    led_a: (RA, GA, BA),
    led_b: (RB, GB, BB),
}

impl<W, R, RA, GA, BA, RB, GB, BB> App<W, R, RA, GA, BA, RB, GB, BB>
where
    W: Write,
    R: Read,
    RA: StatefulOutputPin,
    GA: StatefulOutputPin,
    BA: StatefulOutputPin,
    RB: StatefulOutputPin,
    GB: StatefulOutputPin,
    BB: StatefulOutputPin,
{
    pub fn new(
        to_ctl: W,
        from_ctl: R,
        to_ui: W,
        from_ui: R,
        to_net: W,
        from_net: R,
        led_a: (RA, GA, BA),
        led_b: (RB, GB, BB),
    ) -> Self {
        Self {
            to_ctl,
            to_ui,
            to_net,
            from_ctl,
            from_ui,
            from_net,
            led_a,
            led_b,
        }
    }

    #[allow(unreachable_code)]
    pub async fn run(self) -> ! {
        info!("mgmt: starting");

        let Self {
            mut to_ctl,
            mut to_ui,
            mut to_net,
            from_ctl,
            from_ui,
            from_net,
            led_a,
            led_b,
        } = self;

        // Initialize LEDs
        let mut led_a = Led::new(led_a.0, led_a.1, led_a.2);
        let mut led_b = Led::new(led_b.0, led_b.1, led_b.2);
        led_a.set(Color::Red);
        led_b.set(Color::Green);

        const MAX_QUEUE_DEPTH: usize = 2;
        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let ctl_read_task = read_tlv_loop(from_ctl, channel.sender(), Event::Ctl);
        let ui_read_task = read_raw_loop(from_ui, channel.sender(), Event::Ui);
        let net_read_task = read_raw_loop(from_net, channel.sender(), Event::Net);

        let handle_task = async {
            info!("mgmt: ready to handle events");
            loop {
                match channel.receive().await {
                    Event::Ctl(tlv) => handle_ctl(tlv, &mut to_ctl, &mut to_ui, &mut to_net).await,
                    Event::Ui(data) => to_ctl.must_write_tlv(MgmtToCtl::FromUi, &data).await,
                    Event::Net(data) => to_ctl.must_write_tlv(MgmtToCtl::FromNet, &data).await,
                }
            }
        };

        futures::join!(ctl_read_task, ui_read_task, net_read_task, handle_task);
        unreachable!()
    }
}

async fn handle_ctl<C, U, N>(tlv: Tlv<CtlToMgmt>, to_ctl: &mut C, to_ui: &mut U, to_net: &mut N)
where
    C: WriteTlv<MgmtToCtl>,
    U: WriteTlv<MgmtToUi> + Write,
    N: WriteTlv<MgmtToNet> + Write,
{
    match tlv.tlv_type {
        CtlToMgmt::Ping => {
            info!("mgmt: ctl ping, sending pong");
            to_ctl.must_write_tlv(MgmtToCtl::Pong, &tlv.value).await;
        }
        CtlToMgmt::ToUi => {
            info!("mgmt: ctl -> ui");
            to_ui.write_all(&SYNC_WORD).await.unwrap();
            to_ui.write_all(&tlv.value).await.unwrap();
            to_ui.flush().await.unwrap();
        }
        CtlToMgmt::ToNet => {
            info!("mgmt: ctl -> net");
            to_net.write_all(&SYNC_WORD).await.unwrap();
            to_net.write_all(&tlv.value).await.unwrap();
            to_net.flush().await.unwrap();
        }
    }
}
