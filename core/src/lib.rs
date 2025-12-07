// The code in this crate must be no_std clean.  However, the test code uses std, specifically so
// that tokio can provide an async runtime.
#![cfg_attr(not(test), no_std)]

mod tlv;

// Conditional logging macros - use defmt when feature is enabled, otherwise no-op
#[cfg(feature = "defmt")]
macro_rules! info {
    ($($arg:tt)*) => { defmt::info!($($arg)*) };
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
    R: ReadTlv,
    F: Fn(Tlv<T>) -> E,
{
    loop {
        if let Ok(Some(tlv)) = reader.read_tlv::<T>().await {
            sender.send(wrap(tlv)).await;
        }
        // On error or None, continue looping
    }
}

pub mod mgmt {
    use crate::read_loop;
    use crate::tlv::{
        CtlToMgmt, GuardedReadTlv, GuardedWriteTlv, LabeledReader, LabeledWriter, MgmtToCtl,
        MgmtToNet, MgmtToUi, NetToMgmt, Tlv, UiToMgmt, WriteTlv,
    };
    use embedded_hal::digital::{InputPin, OutputPin};
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        type ToCtl: WriteTlv;
        type ToUi: WriteTlv + Write;
        type ToNet: WriteTlv + Write;

        fn to_ctl(&mut self) -> &mut Self::ToCtl;
        fn to_ui(&mut self) -> &mut Self::ToUi;
        fn to_net(&mut self) -> &mut Self::ToNet;
    }

    pub struct EnvironmentInstance<C, U, N> {
        to_ctl: C,
        to_ui: U,
        to_net: N,
    }

    impl<C, U, N> EnvironmentInstance<C, U, N> {
        fn new(to_ctl: C, to_ui: U, to_net: N) -> Self {
            Self {
                to_ctl,
                to_ui,
                to_net,
            }
        }
    }

    impl<C, U, N> Environment for EnvironmentInstance<C, U, N>
    where
        C: WriteTlv,
        U: WriteTlv + Write,
        N: WriteTlv + Write,
    {
        type ToCtl = C;
        type ToUi = U;
        type ToNet = N;

        fn to_ctl(&mut self) -> &mut Self::ToCtl {
            &mut self.to_ctl
        }

        fn to_ui(&mut self) -> &mut Self::ToUi {
            &mut self.to_ui
        }

        fn to_net(&mut self) -> &mut Self::ToNet {
            &mut self.to_net
        }
    }

    pub enum Event {
        CtlTlv(Tlv<CtlToMgmt>),
        UiTlv(Tlv<UiToMgmt>),
        NetTlv(Tlv<NetToMgmt>),
    }

    #[derive(Default)]
    pub struct App;

    impl App {
        async fn handle(&mut self, event: Event, env: &mut impl Environment) {
            match event {
                Event::CtlTlv(tlv) => self.handle_ctl_tlv(tlv, env).await,
                Event::UiTlv(tlv) => self.handle_ui_tlv(tlv, env).await,
                Event::NetTlv(tlv) => self.handle_net_tlv(tlv, env).await,
            }
        }

        async fn handle_ctl_tlv(&mut self, tlv: Tlv<CtlToMgmt>, env: &mut impl Environment) {
            match tlv.tlv_type {
                CtlToMgmt::Ping => {
                    info!("mgmt: ctl ping, sending pong");
                    env.to_ctl()
                        .must_write_tlv(MgmtToCtl::Pong, &tlv.value)
                        .await;
                }
                CtlToMgmt::ToUi => {
                    info!("mgmt: ctl -> ui");
                    env.to_ui().write_all(&tlv.value).await.unwrap();
                    env.to_ui().flush().await.unwrap();
                }
                CtlToMgmt::ToNet => {
                    info!("mgmt: ctl -> net");
                    env.to_net().write_all(&tlv.value).await.unwrap();
                    env.to_net().flush().await.unwrap();
                }
                CtlToMgmt::UiFirstCircularPing => {
                    info!("mgmt: ui-first circular ping -> ui");
                    env.to_ui()
                        .must_write_tlv(MgmtToUi::CircularPing, &tlv.value)
                        .await;
                }
                CtlToMgmt::NetFirstCircularPing => {
                    info!("mgmt: net-first circular ping -> net");
                    env.to_net()
                        .must_write_tlv(MgmtToNet::CircularPing, &tlv.value)
                        .await;
                }
            }
        }

        async fn handle_ui_tlv(&mut self, tlv: Tlv<UiToMgmt>, env: &mut impl Environment) {
            match tlv.tlv_type {
                UiToMgmt::Pong => {
                    info!("mgmt: ui pong -> ctl");
                    let tlv = Tlv::encode(UiToMgmt::Pong, &tlv.value).await;
                    env.to_ctl().must_write_tlv(MgmtToCtl::FromUi, &tlv).await;
                }
                UiToMgmt::CircularPing => {
                    info!("mgmt: ui circular ping -> ctl");
                    env.to_ctl()
                        .must_write_tlv(MgmtToCtl::NetFirstCircularPing, &tlv.value)
                        .await;
                }
            }
        }

        async fn handle_net_tlv(&mut self, tlv: Tlv<NetToMgmt>, env: &mut impl Environment) {
            match tlv.tlv_type {
                NetToMgmt::Pong => {
                    info!("mgmt: net pong -> ctl");
                    let tlv = Tlv::encode(NetToMgmt::Pong, &tlv.value).await;
                    env.to_ctl().must_write_tlv(MgmtToCtl::FromNet, &tlv).await;
                }
                NetToMgmt::CircularPing => {
                    info!("mgmt: net circular ping -> ctl");
                    env.to_ctl()
                        .must_write_tlv(MgmtToCtl::UiFirstCircularPing, &tlv.value)
                        .await;
                }
            }
        }
    }

    #[allow(unreachable_code)]
    pub async fn run<W, R, SigOut, SigIn>(
        to_ctl: W,
        from_ctl: R,
        to_ui: W,
        from_ui: R,
        to_net: W,
        from_net: R,
        signal_to_net: SigOut,
        signal_from_net: SigIn,
    ) where
        W: Write,
        R: Read,
        SigOut: OutputPin,
        SigIn: InputPin,
    {
        use crate::{Channel, RawMutex};

        info!("mgmt: starting");

        const MAX_QUEUE_DEPTH: usize = 2;

        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mut app = App::default();
        // Wrap writers with labels
        let labeled_to_ctl = LabeledWriter::new("mgmt->ctl", to_ctl);
        let labeled_to_ui = LabeledWriter::new("mgmt->ui", to_ui);
        // Use guarded write for NET - signal high during writes
        let guarded_to_net = GuardedWriteTlv::new("mgmt->net", signal_to_net, to_net);
        let mut env = EnvironmentInstance::new(labeled_to_ctl, labeled_to_ui, guarded_to_net);

        // Wrap readers with labels
        let labeled_from_ctl = LabeledReader::new("ctl->mgmt", from_ctl);
        let labeled_from_ui = LabeledReader::new("ui->mgmt", from_ui);
        let ctl_read_task = read_loop(labeled_from_ctl, channel.sender(), Event::CtlTlv);
        let ui_read_task = read_loop(labeled_from_ui, channel.sender(), Event::UiTlv);

        // Use guarded read for NET - waits for signal, scans for sync word, then reads TLV
        let guarded_from_net = GuardedReadTlv::new("net->mgmt", signal_from_net, from_net);
        let net_read_task = read_loop(guarded_from_net, channel.sender(), Event::NetTlv);

        let handle_task = async {
            info!("mgmt: ready to handle events");
            loop {
                let event = channel.receive().await;
                app.handle(event, &mut env).await;
            }
        };

        futures::join!(ctl_read_task, ui_read_task, net_read_task, handle_task);
    }
}

pub mod ui {
    use crate::read_loop;
    use crate::tlv::{
        LabeledReader, LabeledWriter, MgmtToUi, NetToUi, Tlv, UiToMgmt, UiToNet, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        type ToMgmt: WriteTlv;
        type ToNet: WriteTlv;

        fn to_mgmt(&mut self) -> &mut Self::ToMgmt;
        fn to_net(&mut self) -> &mut Self::ToNet;
    }

    pub struct EnvironmentInstance<M, N> {
        to_mgmt: M,
        to_net: N,
    }

    impl<M, N> EnvironmentInstance<M, N> {
        fn new(to_mgmt: M, to_net: N) -> Self {
            Self { to_mgmt, to_net }
        }
    }

    impl<M, N> Environment for EnvironmentInstance<M, N>
    where
        M: WriteTlv,
        N: WriteTlv,
    {
        type ToMgmt = M;
        type ToNet = N;

        fn to_mgmt(&mut self) -> &mut Self::ToMgmt {
            &mut self.to_mgmt
        }

        fn to_net(&mut self) -> &mut Self::ToNet {
            &mut self.to_net
        }
    }

    pub enum Event {
        MgmtTlv(Tlv<MgmtToUi>),
        NetTlv(Tlv<NetToUi>),
    }

    #[derive(Default)]
    pub struct App;

    impl App {
        async fn handle(&mut self, event: Event, env: &mut impl Environment) {
            match event {
                Event::MgmtTlv(tlv) => self.handle_mgmt_tlv(tlv, env).await,
                Event::NetTlv(tlv) => self.handle_net_tlv(tlv, env).await,
            }
        }

        async fn handle_mgmt_tlv(&mut self, tlv: Tlv<MgmtToUi>, env: &mut impl Environment) {
            match tlv.tlv_type {
                MgmtToUi::Ping => {
                    info!("ui: mgmt ping, sending pong");
                    env.to_mgmt()
                        .must_write_tlv(UiToMgmt::Pong, &tlv.value)
                        .await;
                }
                MgmtToUi::CircularPing => {
                    info!("ui: mgmt circular ping -> net");
                    env.to_net()
                        .must_write_tlv(UiToNet::CircularPing, &tlv.value)
                        .await;
                }
            }
        }

        async fn handle_net_tlv(&mut self, tlv: Tlv<NetToUi>, env: &mut impl Environment) {
            match tlv.tlv_type {
                NetToUi::CircularPing => {
                    info!("ui: net circular ping -> mgmt");
                    env.to_mgmt()
                        .must_write_tlv(UiToMgmt::CircularPing, &tlv.value)
                        .await;
                }
            }
        }
    }

    #[allow(unreachable_code)]
    pub async fn run<W, R>(to_mgmt: W, from_mgmt: R, to_net: W, from_net: R)
    where
        W: Write,
        R: Read,
    {
        use crate::{Channel, RawMutex};

        info!("ui: starting");

        const MAX_QUEUE_DEPTH: usize = 2;

        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mut app = App::default();
        // Wrap writers with labels
        let labeled_to_mgmt = LabeledWriter::new("ui->mgmt", to_mgmt);
        let labeled_to_net = LabeledWriter::new("ui->net", to_net);
        let mut env = EnvironmentInstance::new(labeled_to_mgmt, labeled_to_net);

        // Wrap readers with labels
        let labeled_from_mgmt = LabeledReader::new("mgmt->ui", from_mgmt);
        let labeled_from_net = LabeledReader::new("net->ui", from_net);
        let mgmt_read_task = read_loop(labeled_from_mgmt, channel.sender(), Event::MgmtTlv);
        let net_read_task = read_loop(labeled_from_net, channel.sender(), Event::NetTlv);

        let handle_task = async {
            info!("ui: ready to handle events");
            loop {
                let event = channel.receive().await;
                app.handle(event, &mut env).await;
            }
        };

        futures::join!(mgmt_read_task, net_read_task, handle_task);
    }
}

pub mod net {
    use crate::read_loop;
    use crate::tlv::{
        GuardedReadTlv, GuardedWriteTlv, LabeledReader, LabeledWriter, MgmtToNet, NetToMgmt,
        NetToUi, Tlv, UiToNet, WriteTlv,
    };
    use embedded_hal::digital::{InputPin, OutputPin};
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        type ToMgmt: WriteTlv;
        type ToUi: WriteTlv;

        fn to_mgmt(&mut self) -> &mut Self::ToMgmt;
        fn to_ui(&mut self) -> &mut Self::ToUi;
    }

    pub struct EnvironmentInstance<M, U> {
        to_mgmt: M,
        to_ui: U,
    }

    impl<M, U> EnvironmentInstance<M, U> {
        fn new(to_mgmt: M, to_ui: U) -> Self {
            Self { to_mgmt, to_ui }
        }
    }

    impl<M, U> Environment for EnvironmentInstance<M, U>
    where
        M: WriteTlv,
        U: WriteTlv,
    {
        type ToMgmt = M;
        type ToUi = U;

        fn to_mgmt(&mut self) -> &mut Self::ToMgmt {
            &mut self.to_mgmt
        }

        fn to_ui(&mut self) -> &mut Self::ToUi {
            &mut self.to_ui
        }
    }

    pub enum Event {
        MgmtTlv(Tlv<MgmtToNet>),
        UiTlv(Tlv<UiToNet>),
    }

    #[derive(Default)]
    pub struct App;

    impl App {
        async fn handle(&mut self, event: Event, env: &mut impl Environment) {
            match event {
                Event::MgmtTlv(tlv) => self.handle_mgmt_tlv(tlv, env).await,
                Event::UiTlv(tlv) => self.handle_ui_tlv(tlv, env).await,
            }
        }

        async fn handle_mgmt_tlv(&mut self, tlv: Tlv<MgmtToNet>, env: &mut impl Environment) {
            match tlv.tlv_type {
                MgmtToNet::Ping => {
                    info!("net: mgmt ping, sending pong");
                    env.to_mgmt()
                        .must_write_tlv(NetToMgmt::Pong, &tlv.value)
                        .await
                }

                MgmtToNet::CircularPing => {
                    info!("net: mgmt circular ping -> ui");
                    env.to_ui()
                        .must_write_tlv(NetToUi::CircularPing, &tlv.value)
                        .await;
                }
            }
        }

        async fn handle_ui_tlv(&mut self, tlv: Tlv<UiToNet>, env: &mut impl Environment) {
            match tlv.tlv_type {
                UiToNet::CircularPing => {
                    info!("net: ui circular ping -> mgmt");
                    env.to_mgmt()
                        .must_write_tlv(NetToMgmt::CircularPing, &tlv.value)
                        .await;
                }
            }
        }
    }

    #[allow(unreachable_code)]
    pub async fn run<W, R, SigOut, SigIn>(
        to_mgmt: W,
        from_mgmt: R,
        to_ui: W,
        from_ui: R,
        signal_to_mgmt: SigOut,
        signal_from_mgmt: SigIn,
    ) where
        W: Write,
        R: Read,
        SigOut: OutputPin,
        SigIn: InputPin,
    {
        use crate::{Channel, RawMutex};

        info!("net: starting");

        const MAX_QUEUE_DEPTH: usize = 2;

        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mut app = App::default();
        // Use guarded write for MGMT - signal high during writes (includes label internally)
        let guarded_to_mgmt = GuardedWriteTlv::new("net->mgmt", signal_to_mgmt, to_mgmt);
        // Wrap UI writer with label
        let labeled_to_ui = LabeledWriter::new("net->ui", to_ui);
        let mut env = EnvironmentInstance::new(guarded_to_mgmt, labeled_to_ui);

        // Use guarded read for MGMT - wait for signal before reading (includes label internally)
        let guarded_from_mgmt = GuardedReadTlv::new("mgmt->net", signal_from_mgmt, from_mgmt);
        let mgmt_read_task = read_loop(guarded_from_mgmt, channel.sender(), Event::MgmtTlv);
        // Wrap UI reader with label
        let labeled_from_ui = LabeledReader::new("ui->net", from_ui);
        let ui_read_task = read_loop(labeled_from_ui, channel.sender(), Event::UiTlv);

        let handle_task = async {
            info!("net: ready to handle events");
            loop {
                let event = channel.receive().await;
                app.handle(event, &mut env).await;
            }
        };

        futures::join!(mgmt_read_task, ui_read_task, handle_task);
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

            let tlv: Tlv<UiToMgmt> = tlv.value.as_slice().must_read_tlv().await;
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
            self.to_mgmt
                .must_write_tlv(CtlToMgmt::UiFirstCircularPing, data)
                .await;

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
            assert_eq!(tlv.tlv_type, MgmtToCtl::NetFirstCircularPing);
            assert_eq!(&tlv.value, data);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use core::future::Future;
    use embedded_io_adapters::futures_03::FromFutures;
    use tokio::sync::mpsc;

    type Reader = FromFutures<async_ringbuffer::Reader>;
    type Writer = FromFutures<async_ringbuffer::Writer>;

    fn channel() -> (Writer, Reader) {
        const BUFFER_CAPACITY: usize = 1024;
        let (w, r) = async_ringbuffer::ring_buffer(BUFFER_CAPACITY);
        (FromFutures::new(w), FromFutures::new(r))
    }

    /// Mock output pin that sends value changes over a channel
    struct MockOutputPin {
        tx: mpsc::Sender<bool>,
        current: bool,
    }

    impl MockOutputPin {
        fn new(tx: mpsc::Sender<bool>) -> Self {
            Self { tx, current: false }
        }
    }

    impl embedded_hal::digital::ErrorType for MockOutputPin {
        type Error = core::convert::Infallible;
    }

    impl embedded_hal::digital::OutputPin for MockOutputPin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            if self.current {
                self.current = false;
                let _ = self.tx.try_send(false);
            }
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            if !self.current {
                self.current = true;
                let _ = self.tx.try_send(true);
            }
            Ok(())
        }
    }

    /// Mock input pin that receives value changes from a channel
    struct MockInputPin {
        rx: mpsc::Receiver<bool>,
        current: bool,
    }

    impl MockInputPin {
        fn new(rx: mpsc::Receiver<bool>) -> Self {
            Self { rx, current: false }
        }
    }

    impl embedded_hal::digital::ErrorType for MockInputPin {
        type Error = core::convert::Infallible;
    }

    impl embedded_hal::digital::InputPin for MockInputPin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            // Check for any pending updates (non-blocking)
            while let Ok(val) = self.rx.try_recv() {
                self.current = val;
            }
            Ok(self.current)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.is_high()?)
        }
    }

    impl embedded_hal_async::digital::Wait for MockInputPin {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            while !self.current {
                if let Some(val) = self.rx.recv().await {
                    self.current = val;
                }
            }
            Ok(())
        }

        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            while self.current {
                if let Some(val) = self.rx.recv().await {
                    self.current = val;
                }
            }
            Ok(())
        }

        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            self.wait_for_low().await?;
            self.wait_for_high().await
        }

        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            self.wait_for_high().await?;
            self.wait_for_low().await
        }

        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            if let Some(val) = self.rx.recv().await {
                self.current = val;
            }
            Ok(())
        }
    }

    /// Create a mock signal pair (output pin, input pin)
    fn signal_channel() -> (MockOutputPin, MockInputPin) {
        let (tx, rx) = mpsc::channel(1);
        (MockOutputPin::new(tx), MockInputPin::new(rx))
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

        // Signal channels for MGMT <-> NET synchronization
        let (mgmt_signal_to_net, net_signal_from_mgmt) = signal_channel();
        let (net_signal_to_mgmt, mgmt_signal_from_net) = signal_channel();

        let ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
            mgmt_signal_to_net,
            mgmt_signal_from_net,
        );
        let ui_task = ui::run(ui_to_mgmt, ui_from_mgmt, ui_to_net, ui_from_net);
        let net_task = net::run(
            net_to_mgmt,
            net_from_mgmt,
            net_to_ui,
            net_from_ui,
            net_signal_to_mgmt,
            net_signal_from_mgmt,
        );

        tokio::select! {
            _ = test_fn(ctl_app) => {},
            _ = mgmt_task => {},
            _ = ui_task => {},
            _ = net_task => {},
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
