// The code in this crate must be no_std clean.  However, the test code uses std, specifically so
// that tokio can provide an async runtime.
// #![cfg_attr(not(test), no_std)]

mod tlv;

// Conditional logging macros - use defmt when feature is enabled, otherwise no-op
#[cfg(feature = "defmt")]
macro_rules! info {
    ($($arg:tt)*) => { println!($($arg)*) };
    // ($($arg:tt)*) => { defmt::info!($($arg)*) };
}

#[cfg(not(feature = "defmt"))]
macro_rules! info {
    ($($arg:tt)*) => {};
}

use crate::tlv::{ReadTlv, Tlv};
use embassy_sync::channel::{Channel, Sender};

type RawMutex = embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

async fn read_loop<'a, T, R, E, F, const N: usize>(
    mut reader: R,
    sender: Sender<'a, RawMutex, E, N>,
    wrap: F,
) -> !
where
    T: TryFrom<u16>,
    R: ReadTlv<T>,
    F: Fn(Tlv<T>) -> E,
{
    loop {
        if let Ok(Some(tlv)) = reader.read_tlv().await {
            sender.send(wrap(tlv)).await;
        }
        // On error or None, continue looping
    }
}

pub mod mgmt {
    use crate::read_loop;
    use crate::tlv::{
        CtlToMgmt, LabeledReader, LabeledWriter, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, Tlv,
        UiToMgmt, Value, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    enum Event {
        Ctl(Tlv<CtlToMgmt>),
        Ui(Value),
        Net(Tlv<NetToMgmt>),
    }

    pub struct App<W, R> {
        to_ctl: LabeledWriter<W>,
        to_ui: LabeledWriter<W>,
        to_net: LabeledWriter<W>,
        from_ctl: LabeledReader<R>,
        from_ui: LabeledReader<R>,
        from_net: LabeledReader<R>,
    }

    impl<W, R> App<W, R>
    where
        W: Write,
        R: Read,
    {
        pub fn new(to_ctl: W, from_ctl: R, to_ui: W, from_ui: R, to_net: W, from_net: R) -> Self {
            Self {
                to_ctl: LabeledWriter::new("mgmt->ctl", to_ctl),
                to_ui: LabeledWriter::new("mgmt->ui", to_ui),
                to_net: LabeledWriter::new("mgmt->net", to_net),
                from_ctl: LabeledReader::new("ctl->mgmt", from_ctl),
                from_ui: LabeledReader::new("ui->mgmt", from_ui),
                from_net: LabeledReader::new("net->mgmt", from_net),
            }
        }

        #[allow(unreachable_code)]
        pub async fn run(self) -> ! {
            use crate::{Channel, RawMutex};

            info!("mgmt: starting");

            let Self {
                mut to_ctl,
                mut to_ui,
                mut to_net,
                from_ctl,
                mut from_ui,
                from_net,
            } = self;

            const MAX_QUEUE_DEPTH: usize = 2;
            let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

            let ctl_read_task = read_loop(from_ctl, channel.sender(), Event::Ctl);
            // let ui_read_task = read_loop(from_ui, channel.sender(), Event::Ui);
            let net_read_task = read_loop(from_net, channel.sender(), Event::Net);

            let ui_sender_raw = channel.sender();
            let ui_read_task_raw = async {
                let mut buffer = [0u8; crate::tlv::MAX_VALUE_SIZE];
                loop {
                    let Ok(n) = from_ui.read(&mut buffer).await else {
                        continue;
                    };

                    if n == 0 {
                        continue;
                    }

                    let value = buffer[..n].try_into().unwrap();
                    ui_sender_raw.send(Event::Ui(value)).await;
                }
            };

            let handle_task = async {
                info!("mgmt: ready to handle events");
                loop {
                    match channel.receive().await {
                        Event::Ctl(tlv) => {
                            handle_ctl(tlv, &mut to_ctl, &mut to_ui, &mut to_net).await
                        }
                        Event::Ui(value) => handle_ui(&value, &mut to_ctl).await,
                        Event::Net(tlv) => handle_net(tlv, &mut to_ctl).await,
                    }
                }
            };

            futures::join!(ctl_read_task, ui_read_task_raw, net_read_task, handle_task);
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
                to_ui.write_all(&tlv.value).await.unwrap();
                to_ui.flush().await.unwrap();
            }
            CtlToMgmt::ToNet => {
                info!("mgmt: ctl -> net");
                to_net.write_all(&tlv.value).await.unwrap();
                to_net.flush().await.unwrap();
            }
            CtlToMgmt::UiFirstCircularPing => {
                info!("mgmt: ui-first circular ping -> ui");
                to_ui
                    .must_write_tlv(MgmtToUi::CircularPing, &tlv.value)
                    .await;
            }
            CtlToMgmt::NetFirstCircularPing => {
                info!("mgmt: net-first circular ping -> net");
                to_net
                    .must_write_tlv(MgmtToNet::CircularPing, &tlv.value)
                    .await;
            }
        }
    }

    async fn handle_ui<C>(data: &[u8], to_ctl: &mut C)
    where
        C: WriteTlv<MgmtToCtl>,
    {
        to_ctl.must_write_tlv(MgmtToCtl::FromUi, &data).await;
    }

    async fn handle_net<C>(tlv: Tlv<NetToMgmt>, to_ctl: &mut C)
    where
        C: WriteTlv<MgmtToCtl>,
    {
        match tlv.tlv_type {
            NetToMgmt::Pong => {
                info!("mgmt: net pong -> ctl");
                let encoded = Tlv::encode(NetToMgmt::Pong, &tlv.value).await;
                to_ctl.must_write_tlv(MgmtToCtl::FromNet, &encoded).await;
            }
            NetToMgmt::CircularPing => {
                info!("mgmt: net circular ping -> ctl");
                to_ctl
                    .must_write_tlv(MgmtToCtl::UiFirstCircularPing, &tlv.value)
                    .await;
            }
        }
    }
}

pub mod ui {
    use crate::read_loop;
    use crate::tlv::{
        LabeledReader, LabeledWriter, MgmtToUi, NetToUi, Tlv, UiToMgmt, UiToNet, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    enum Event {
        Mgmt(Tlv<MgmtToUi>),
        Net(Tlv<NetToUi>),
    }

    pub struct App<W, R> {
        to_mgmt: LabeledWriter<W>,
        to_net: LabeledWriter<W>,
        from_mgmt: LabeledReader<R>,
        from_net: LabeledReader<R>,
    }

    impl<W, R> App<W, R>
    where
        W: Write,
        R: Read,
    {
        pub fn new(to_mgmt: W, from_mgmt: R, to_net: W, from_net: R) -> Self {
            Self {
                to_mgmt: LabeledWriter::new("ui->mgmt", to_mgmt),
                to_net: LabeledWriter::new("ui->net", to_net),
                from_mgmt: LabeledReader::new("mgmt->ui", from_mgmt),
                from_net: LabeledReader::new("net->ui", from_net),
            }
        }

        #[allow(unreachable_code)]
        pub async fn run(self) -> ! {
            use crate::{Channel, RawMutex};

            info!("ui: starting");

            let Self {
                mut to_mgmt,
                mut to_net,
                from_mgmt,
                from_net,
            } = self;

            const MAX_QUEUE_DEPTH: usize = 2;
            let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

            let mgmt_read_task = read_loop(from_mgmt, channel.sender(), Event::Mgmt);
            let net_read_task = read_loop(from_net, channel.sender(), Event::Net);

            let handle_task = async {
                info!("ui: ready to handle events");
                loop {
                    match channel.receive().await {
                        Event::Mgmt(tlv) => handle_mgmt(tlv, &mut to_mgmt, &mut to_net).await,
                        Event::Net(tlv) => handle_net(tlv, &mut to_mgmt).await,
                    }
                }
            };

            futures::join!(mgmt_read_task, net_read_task, handle_task);
            unreachable!()
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
}

pub mod net {
    use crate::read_loop;
    use crate::tlv::{
        LabeledReader, LabeledWriter, MgmtToNet, NetToMgmt, NetToUi, Tlv, UiToNet, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    enum Event {
        Mgmt(Tlv<MgmtToNet>),
        Ui(Tlv<UiToNet>),
    }

    pub struct App<W, R> {
        to_mgmt: LabeledWriter<W>,
        to_ui: LabeledWriter<W>,
        from_mgmt: LabeledReader<R>,
        from_ui: LabeledReader<R>,
    }

    impl<W, R> App<W, R>
    where
        W: Write,
        R: Read,
    {
        pub fn new(to_mgmt: W, from_mgmt: R, to_ui: W, from_ui: R) -> Self {
            Self {
                to_mgmt: LabeledWriter::new("net->mgmt", to_mgmt),
                to_ui: LabeledWriter::new("net->ui", to_ui),
                from_mgmt: LabeledReader::new("mgmt->net", from_mgmt),
                from_ui: LabeledReader::new("ui->net", from_ui),
            }
        }

        #[allow(unreachable_code)]
        pub async fn run(self) -> ! {
            use crate::{Channel, RawMutex};

            info!("net: starting");

            let Self {
                mut to_mgmt,
                mut to_ui,
                from_mgmt,
                from_ui,
            } = self;

            const MAX_QUEUE_DEPTH: usize = 2;
            let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

            let mgmt_read_task = read_loop(from_mgmt, channel.sender(), Event::Mgmt);
            let ui_read_task = read_loop(from_ui, channel.sender(), Event::Ui);

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
}

pub mod ctl {
    use crate::tlv::{
        CtlToMgmt, LabeledReader, LabeledWriter, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt,
        ReadTlv, Tlv, UiToMgmt, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    pub struct App<R, W> {
        to_mgmt: LabeledWriter<W>,
        from_mgmt: LabeledReader<R>,
    }

    impl<R, W> App<R, W> {
        pub fn new(to_mgmt: W, from_mgmt: R) -> Self {
            Self {
                to_mgmt: LabeledWriter::new("ctl->mgmt", to_mgmt),
                from_mgmt: LabeledReader::new("mgmt->ctl", from_mgmt),
            }
        }

        pub async fn send_mgmt_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            self.to_mgmt.must_write_tlv(CtlToMgmt::Ping, data).await;

            let tlv: Tlv<MgmtToCtl> = self.from_mgmt.must_read_tlv().await;

            assert_eq!(tlv.tlv_type, MgmtToCtl::Pong);
            assert_eq!(&tlv.value, data);
        }

        pub async fn send_ui_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            let payload = Tlv::encode(MgmtToUi::Ping, data).await;
            self.to_mgmt.must_write_tlv(CtlToMgmt::ToUi, &payload).await;

            let tlv: Tlv<MgmtToCtl> = self.from_mgmt.must_read_tlv().await;
            assert_eq!(tlv.tlv_type, MgmtToCtl::FromUi);

            let mut tlv_reader = LabeledReader::new("ui->ctl", tlv.value.as_slice());
            let tlv: Tlv<UiToMgmt> = tlv_reader.must_read_tlv().await;
            assert_eq!(tlv.tlv_type, UiToMgmt::Pong);
            assert_eq!(&tlv.value, data);
        }

        pub async fn send_net_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            let payload = Tlv::encode(MgmtToNet::Ping, data).await;
            self.to_mgmt
                .must_write_tlv(CtlToMgmt::ToNet, &payload)
                .await;

            let tlv: Tlv<MgmtToCtl> = self.from_mgmt.must_read_tlv().await;
            assert_eq!(tlv.tlv_type, MgmtToCtl::FromNet);

            let tlv: Tlv<NetToMgmt> = tlv.value.as_slice().must_read_tlv().await;
            assert_eq!(tlv.tlv_type, NetToMgmt::Pong);
            assert_eq!(&tlv.value, data);
        }

        pub async fn ui_first_circular_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            let payload = Tlv::encode(MgmtToUi::CircularPing, data).await;
            self.to_mgmt.must_write_tlv(CtlToMgmt::ToUi, &payload).await;

            let tlv: Tlv<MgmtToCtl> = self.from_mgmt.must_read_tlv().await;
            assert_eq!(tlv.tlv_type, MgmtToCtl::UiFirstCircularPing);
            assert_eq!(&tlv.value, data);
        }

        pub async fn net_first_circular_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            self.to_mgmt
                .must_write_tlv(CtlToMgmt::NetFirstCircularPing, data)
                .await;

            let tlv: Tlv<MgmtToCtl> = self.from_mgmt.must_read_tlv().await;
            assert_eq!(tlv.tlv_type, MgmtToCtl::FromUi);

            let mut tlv_reader = LabeledReader::new("ui->ctl", tlv.value.as_slice());
            let tlv: Tlv<UiToMgmt> = tlv_reader.must_read_tlv().await;
            assert_eq!(tlv.tlv_type, UiToMgmt::CircularPing);
            assert_eq!(&tlv.value, data);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use core::future::Future;
    use embedded_io_adapters::futures_03::FromFutures;

    type Reader = FromFutures<async_ringbuffer::Reader>;
    type Writer = FromFutures<async_ringbuffer::Writer>;

    fn channel() -> (Writer, Reader) {
        const BUFFER_CAPACITY: usize = 1024;
        let (w, r) = async_ringbuffer::ring_buffer(BUFFER_CAPACITY);
        (FromFutures::new(w), FromFutures::new(r))
    }

    async fn device_test<F, Fut>(test_fn: F)
    where
        F: FnOnce(ctl::App<Reader, Writer>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let (ui_to_mgmt, mgmt_from_ui) = channel();
        let (mgmt_to_ui, ui_from_mgmt) = channel();

        let (net_to_mgmt, mgmt_from_net) = channel();
        let (mgmt_to_net, net_from_mgmt) = channel();

        let (net_to_ui, ui_from_net) = channel();
        let (ui_to_net, net_from_ui) = channel();

        let ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_app = mgmt::App::new(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
        );
        let ui_app = ui::App::new(ui_to_mgmt, ui_from_mgmt, ui_to_net, ui_from_net);
        let net_app = net::App::new(net_to_mgmt, net_from_mgmt, net_to_ui, net_from_ui);

        tokio::select! {
            _ = test_fn(ctl_app) => {},
            _ = mgmt_app.run() => {},
            _ = ui_app.run() => {},
            _ = net_app.run() => {},
        }
    }

    #[tokio::test]
    async fn ctl_mgmt_ping() {
        device_test(|mut ctl| async move {
            ctl.send_mgmt_ping(b"hello mgmt").await;
        })
        .await;
    }

    #[tokio::test]
    async fn ctl_mgmt_ui_ping() {
        device_test(|mut ctl| async move {
            ctl.send_ui_ping(b"hello ui").await;
        })
        .await;
    }

    #[tokio::test]
    async fn ctl_mgmt_net_ping() {
        device_test(|mut ctl| async move {
            ctl.send_net_ping(b"hello net").await;
        })
        .await;
    }

    #[tokio::test]
    async fn ui_first_circular_ping() {
        device_test(|mut ctl| async move {
            ctl.ui_first_circular_ping(b"hello ui circular").await;
        })
        .await;
    }

    #[tokio::test]
    async fn net_first_circular_ping() {
        device_test(|mut ctl| async move {
            ctl.net_first_circular_ping(b"hello net circular").await;
        })
        .await;
    }
}
