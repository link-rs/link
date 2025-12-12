//! MGMT (Management) chip - coordinates communication between all chips.

use crate::info;
use crate::shared::{
    Color, CtlToMgmt, Led, MgmtToCtl, MgmtToNet, MgmtToUi, ReadTlv, Tlv, Value, WriteTlv, SYNC_WORD,
};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};

#[allow(unreachable_code)]
pub async fn run<W, R, RA, GA, BA, RB, GB, BB>(
    to_ctl: W,
    mut from_ctl: R,
    mut to_ui: W,
    mut from_ui: R,
    mut to_net: W,
    mut from_net: R,
    led_a: (RA, GA, BA),
    led_b: (RB, GB, BB),
) -> !
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
    use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

    info!("mgmt: starting");

    // Initialize LEDs
    let mut led_a = Led::new(led_a.0, led_a.1, led_a.2);
    let mut led_b = Led::new(led_b.0, led_b.1, led_b.2);
    led_a.set(Color::Red);
    led_b.set(Color::Green);

    let to_ctl: Mutex<NoopRawMutex, _> = Mutex::new(to_ctl);

    let ui_task = async {
        let mut buffer = Value::default();
        loop {
            buffer.resize(buffer.capacity(), 0).unwrap();
            let Ok(n) = from_ui.read(&mut buffer).await else {
                continue;
            };
            buffer.truncate(n);

            let mut to_ctl = to_ctl.lock().await;
            let _ = to_ctl.write_tlv(MgmtToCtl::FromUi, &buffer).await;
        }
    };

    let net_task = async {
        let mut buffer = Value::default();
        loop {
            buffer.resize(buffer.capacity(), 0).unwrap();
            let Ok:n) = from_net.read(&mut buffer).await else {
                continue;
            };
            buffer.truncate(n);

            let mut to_ctl = to_ctl.lock().await;
            let _ = to_ctl.write_tlv(MgmtToCtl::FromNet, &buffer).await;
        }
    };

    let ctl_task = async {
        use core::ops::DerefMut;
        loop {
            let Ok(Some(tlv)) = from_ctl.read_tlv().await else {
                continue;
            };

            let mut to_ctl = to_ctl.lock().await;
            handle_ctl(tlv, to_ctl.deref_mut(), &mut to_ui, &mut to_net).await;
        }
    };

    embassy_futures::join::join3(ctl_task, ui_task, net_task).await;
    unreachable!()
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
