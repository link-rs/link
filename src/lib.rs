// The code in this crate must be no_std clean.  However, the test code uses std, specifically so
// that tokio can provide an async runtime.
#![cfg_attr(not(test), no_std)]

mod tlv;

pub mod mgmt {
    use crate::tlv::{
        CtlToMgmt, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, ReadTlv, Tlv, UiToMgmt, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        fn to_ctl(&mut self) -> &mut impl Write;
        fn to_ui(&mut self) -> &mut impl Write;
        fn to_net(&mut self) -> &mut impl Write;
    }

    pub struct EnvironmentInstance<W> {
        to_ctl: W,
        to_ui: W,
        to_net: W,
    }

    impl<W> EnvironmentInstance<W> {
        fn new(to_ctl: W, to_ui: W, to_net: W) -> Self {
            Self {
                to_ctl,
                to_ui,
                to_net,
            }
        }
    }

    impl<W> Environment for EnvironmentInstance<W>
    where
        W: Write,
    {
        fn to_ctl(&mut self) -> &mut impl Write {
            &mut self.to_ctl
        }

        fn to_ui(&mut self) -> &mut impl Write {
            &mut self.to_ui
        }

        fn to_net(&mut self) -> &mut impl Write {
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
                    env.to_ctl()
                        .write_tlv(MgmtToCtl::Pong, &tlv.value)
                        .await
                        .unwrap();
                }
                CtlToMgmt::ToUi => {
                    env.to_ui().write(&tlv.value).await.unwrap();
                }
                CtlToMgmt::ToNet => {
                    env.to_net().write(&tlv.value).await.unwrap();
                }
                CtlToMgmt::UiFirstCircularPing => {
                    env.to_ui()
                        .write_tlv(MgmtToUi::CircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
                CtlToMgmt::NetFirstCircularPing => {
                    env.to_net()
                        .write_tlv(MgmtToNet::CircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }

        async fn handle_ui_tlv(&mut self, tlv: Tlv<UiToMgmt>, env: &mut impl Environment) {
            match tlv.tlv_type {
                UiToMgmt::Pong => {
                    let tlv = Tlv::encode(UiToMgmt::Pong, &tlv.value).await;
                    env.to_ctl()
                        .write_tlv(MgmtToCtl::FromUi, &tlv)
                        .await
                        .unwrap();
                }
                UiToMgmt::CircularPing => {
                    env.to_ctl()
                        .write_tlv(MgmtToCtl::NetFirstCircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }

        async fn handle_net_tlv(&mut self, tlv: Tlv<NetToMgmt>, env: &mut impl Environment) {
            match tlv.tlv_type {
                NetToMgmt::Pong => {
                    let tlv = Tlv::encode(NetToMgmt::Pong, &tlv.value).await;
                    env.to_ctl()
                        .write_tlv(MgmtToCtl::FromNet, &tlv)
                        .await
                        .unwrap();
                }
                NetToMgmt::CircularPing => {
                    env.to_ctl()
                        .write_tlv(MgmtToCtl::UiFirstCircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }
    }

    pub async fn run<W, R>(
        to_ctl: W,
        mut from_ctl: R,
        to_ui: W,
        mut from_ui: R,
        to_net: W,
        mut from_net: R,
    ) where
        W: Write + 'static,
        R: Read + 'static,
    {
        const MAX_QUEUE_DEPTH: usize = 32;

        let (sender, receiver) = async_channel::bounded::<Event>(MAX_QUEUE_DEPTH);

        let mut app = App::default();
        let mut env = EnvironmentInstance::new(to_ctl, to_ui, to_net);

        // Read threads
        // TODO make these loops more brief and intelligible
        let ctl_sender = sender.clone();
        let ctl_read_task = async move {
            loop {
                let tlv: Tlv<CtlToMgmt> = match from_ctl.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if ctl_sender.send(Event::CtlTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        let ui_sender = sender.clone();
        let ui_read_task = async move {
            loop {
                let tlv: Tlv<UiToMgmt> = match from_ui.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if ui_sender.send(Event::UiTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        let net_sender = sender.clone();
        let net_read_task = async move {
            loop {
                let tlv: Tlv<NetToMgmt> = match from_net.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if net_sender.send(Event::NetTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        // Handle thread
        let handle_task = async move {
            while let Ok(tlv) = receiver.recv().await {
                app.handle(tlv, &mut env).await;
            }
        };

        futures::join!(ctl_read_task, ui_read_task, net_read_task, handle_task);
    }
}

pub mod ui {
    use crate::tlv::{MgmtToUi, NetToUi, ReadTlv, Tlv, UiToMgmt, UiToNet, WriteTlv};
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        fn to_mgmt(&mut self) -> &mut impl Write;
        fn to_net(&mut self) -> &mut impl Write;
    }

    pub struct EnvironmentInstance<W> {
        to_mgmt: W,
        to_net: W,
    }

    impl<W> EnvironmentInstance<W> {
        fn new(to_mgmt: W, to_net: W) -> Self {
            Self { to_mgmt, to_net }
        }
    }

    impl<W> Environment for EnvironmentInstance<W>
    where
        W: Write,
    {
        fn to_mgmt(&mut self) -> &mut impl Write {
            &mut self.to_mgmt
        }

        fn to_net(&mut self) -> &mut impl Write {
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
                    env.to_mgmt()
                        .write_tlv(UiToMgmt::Pong, &tlv.value)
                        .await
                        .unwrap();
                }
                MgmtToUi::CircularPing => {
                    env.to_net()
                        .write_tlv(UiToNet::CircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }

        async fn handle_net_tlv(&mut self, tlv: Tlv<NetToUi>, env: &mut impl Environment) {
            match tlv.tlv_type {
                NetToUi::CircularPing => {
                    env.to_mgmt()
                        .write_tlv(UiToMgmt::CircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }
    }

    pub async fn run<W, R>(to_mgmt: W, mut from_mgmt: R, to_net: W, mut from_net: R)
    where
        W: Write + 'static,
        R: Read + 'static,
    {
        const MAX_QUEUE_DEPTH: usize = 32;

        let (sender, receiver) = async_channel::bounded::<Event>(MAX_QUEUE_DEPTH);

        let mut app = App::default();
        let mut env = EnvironmentInstance::new(to_mgmt, to_net);

        // Read threads
        let mgmt_sender = sender.clone();
        let mgmt_read_task = async move {
            loop {
                let tlv: Tlv<MgmtToUi> = match from_mgmt.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if mgmt_sender.send(Event::MgmtTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        let net_sender = sender.clone();
        let net_read_task = async move {
            loop {
                let tlv: Tlv<NetToUi> = match from_net.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if net_sender.send(Event::NetTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        // Handle thread
        let handle_task = async move {
            while let Ok(tlv) = receiver.recv().await {
                app.handle(tlv, &mut env).await;
            }
        };

        futures::join!(mgmt_read_task, net_read_task, handle_task);
    }
}

pub mod net {
    use crate::tlv::{MgmtToNet, NetToMgmt, NetToUi, ReadTlv, Tlv, UiToNet, WriteTlv};
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        fn to_mgmt(&mut self) -> &mut impl Write;
        fn to_ui(&mut self) -> &mut impl Write;
    }

    pub struct EnvironmentInstance<W> {
        to_mgmt: W,
        to_ui: W,
    }

    impl<W> EnvironmentInstance<W> {
        fn new(to_mgmt: W, to_ui: W) -> Self {
            Self { to_mgmt, to_ui }
        }
    }

    impl<W> Environment for EnvironmentInstance<W>
    where
        W: Write,
    {
        fn to_mgmt(&mut self) -> &mut impl Write {
            &mut self.to_mgmt
        }

        fn to_ui(&mut self) -> &mut impl Write {
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
                    env.to_mgmt()
                        .write_tlv(NetToMgmt::Pong, &tlv.value)
                        .await
                        .unwrap();
                }
                MgmtToNet::CircularPing => {
                    env.to_ui()
                        .write_tlv(NetToUi::CircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }

        async fn handle_ui_tlv(&mut self, tlv: Tlv<UiToNet>, env: &mut impl Environment) {
            match tlv.tlv_type {
                UiToNet::CircularPing => {
                    env.to_mgmt()
                        .write_tlv(NetToMgmt::CircularPing, &tlv.value)
                        .await
                        .unwrap();
                }
            }
        }
    }

    pub async fn run<W, R>(to_mgmt: W, mut from_mgmt: R, to_ui: W, mut from_ui: R)
    where
        W: Write + 'static,
        R: Read + 'static,
    {
        const MAX_QUEUE_DEPTH: usize = 32;

        let (sender, receiver) = async_channel::bounded::<Event>(MAX_QUEUE_DEPTH);

        let mut app = App::default();
        let mut env = EnvironmentInstance::new(to_mgmt, to_ui);

        // Read thread
        let mgmt_sender = sender.clone();
        let mgmt_read_task = async move {
            loop {
                let tlv: Tlv<MgmtToNet> = match from_mgmt.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if mgmt_sender.send(Event::MgmtTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        let ui_sender = sender.clone();
        let ui_read_task = async move {
            loop {
                let tlv: Tlv<UiToNet> = match from_ui.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if ui_sender.send(Event::UiTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        // Handle thread
        let handle_task = async move {
            while let Ok(tlv) = receiver.recv().await {
                app.handle(tlv, &mut env).await;
            }
        };

        futures::join!(mgmt_read_task, ui_read_task, handle_task);
    }
}

pub mod ctl {
    use crate::tlv::{
        CtlToMgmt, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, ReadTlv, Tlv, UiToMgmt, WriteTlv,
    };
    use embedded_io_async::{Read, Write};

    pub struct App<R, W> {
        to_mgmt: W,
        from_mgmt: R,
    }

    impl<R, W> App<R, W> {
        pub fn new(to_mgmt: W, from_mgmt: R) -> Self {
            Self { to_mgmt, from_mgmt }
        }

        pub async fn send_mgmt_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            self.to_mgmt.write_tlv(CtlToMgmt::Ping, data).await.unwrap();

            let tlv: Tlv<MgmtToCtl> = match self.from_mgmt.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, MgmtToCtl::Pong);
            assert_eq!(&tlv.value, data);
        }

        pub async fn send_ui_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            let payload = Tlv::encode(MgmtToUi::Ping, data).await;
            self.to_mgmt
                .write_tlv(CtlToMgmt::ToUi, &payload)
                .await
                .unwrap();

            let tlv: Tlv<MgmtToCtl> = match self.from_mgmt.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, MgmtToCtl::FromUi);

            let tlv: Tlv<UiToMgmt> = match tlv.value.as_slice().read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

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
                .write_tlv(CtlToMgmt::ToNet, &payload)
                .await
                .unwrap();

            let tlv: Tlv<MgmtToCtl> = match self.from_mgmt.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, MgmtToCtl::FromNet);

            let tlv: Tlv<NetToMgmt> = match tlv.value.as_slice().read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, NetToMgmt::Pong);
            assert_eq!(&tlv.value, data);
        }

        pub async fn ui_first_circular_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            self.to_mgmt
                .write_tlv(CtlToMgmt::UiFirstCircularPing, data)
                .await
                .unwrap();

            let tlv: Tlv<MgmtToCtl> = match self.from_mgmt.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, MgmtToCtl::UiFirstCircularPing);
            assert_eq!(&tlv.value, data);
        }

        pub async fn net_first_circular_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            self.to_mgmt
                .write_tlv(CtlToMgmt::NetFirstCircularPing, data)
                .await
                .unwrap();

            let tlv: Tlv<MgmtToCtl> = match self.from_mgmt.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, MgmtToCtl::NetFirstCircularPing);
            assert_eq!(&tlv.value, data);
        }
    }
}

#[cfg(test)]
mod test {
    extern crate std;

    use super::*;
    use embedded_io_adapters::futures_03::FromFutures;

    type Reader = FromFutures<async_ringbuffer::Reader>;
    type Writer = FromFutures<async_ringbuffer::Writer>;

    fn channel() -> (Writer, Reader) {
        const BUFFER_CAPACITY: usize = 1024;
        let (w, r) = async_ringbuffer::ring_buffer(BUFFER_CAPACITY);
        (FromFutures::new(w), FromFutures::new(r))
    }

    #[tokio::test]
    async fn ctl_mgmt_ping() {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let (_ui_to_mgmt, mgmt_from_ui) = channel();
        let (mgmt_to_ui, _ui_from_mgmt) = channel();

        let (_net_to_mgmt, mgmt_from_net) = channel();
        let (mgmt_to_net, _net_from_mgmt) = channel();

        let mut ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
        );

        let write_data = b"hello mgmt";
        tokio::select!(
            _ = ctl_app.send_mgmt_ping(write_data) => {},
            _ = mgmt_task => {},
        );
    }

    #[tokio::test]
    async fn ctl_mgmt_ui_ping() {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let (ui_to_mgmt, mgmt_from_ui) = channel();
        let (mgmt_to_ui, ui_from_mgmt) = channel();

        let (_net_to_mgmt, mgmt_from_net) = channel();
        let (mgmt_to_net, _net_from_mgmt) = channel();

        let (_net_to_ui, ui_from_net) = channel();
        let (ui_to_net, _net_from_ui) = channel();

        let mut ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
        );
        let ui_task = ui::run(ui_to_mgmt, ui_from_mgmt, ui_to_net, ui_from_net);

        let write_data = b"hello ui";
        tokio::select!(
            _ = ctl_app.send_ui_ping(write_data) => {},
            _ = mgmt_task => {},
            _ = ui_task => {},
        );
    }

    #[tokio::test]
    async fn ctl_mgmt_net_ping() {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let (_ui_to_mgmt, mgmt_from_ui) = channel();
        let (mgmt_to_ui, _ui_from_mgmt) = channel();

        let (net_to_mgmt, mgmt_from_net) = channel();
        let (mgmt_to_net, net_from_mgmt) = channel();

        let (net_to_ui, _ui_from_net) = channel();
        let (_ui_to_net, net_from_ui) = channel();

        let mut ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
        );
        let net_task = net::run(net_to_mgmt, net_from_mgmt, net_to_ui, net_from_ui);

        let write_data = b"hello net";
        tokio::select!(
            _ = ctl_app.send_net_ping(write_data) => {},
            _ = mgmt_task => {},
            _ = net_task => {},
        );
    }

    #[tokio::test]
    async fn ui_first_circular_ping() {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let (ui_to_mgmt, mgmt_from_ui) = channel();
        let (mgmt_to_ui, ui_from_mgmt) = channel();

        let (net_to_mgmt, mgmt_from_net) = channel();
        let (mgmt_to_net, net_from_mgmt) = channel();

        let (net_to_ui, ui_from_net) = channel();
        let (ui_to_net, net_from_ui) = channel();

        let mut ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
        );
        let ui_task = ui::run(ui_to_mgmt, ui_from_mgmt, ui_to_net, ui_from_net);
        let net_task = net::run(net_to_mgmt, net_from_mgmt, net_to_ui, net_from_ui);

        let write_data = b"hello ui circular";
        tokio::select!(
            _ = ctl_app.ui_first_circular_ping(write_data) => {},
            _ = mgmt_task => {},
            _ = net_task => {},
            _ = ui_task => {},
        );
    }

    #[tokio::test]
    async fn net_first_circular_ping() {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let (ui_to_mgmt, mgmt_from_ui) = channel();
        let (mgmt_to_ui, ui_from_mgmt) = channel();

        let (net_to_mgmt, mgmt_from_net) = channel();
        let (mgmt_to_net, net_from_mgmt) = channel();

        let (net_to_ui, ui_from_net) = channel();
        let (ui_to_net, net_from_ui) = channel();

        let mut ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(
            mgmt_to_ctl,
            mgmt_from_ctl,
            mgmt_to_ui,
            mgmt_from_ui,
            mgmt_to_net,
            mgmt_from_net,
        );
        let ui_task = ui::run(ui_to_mgmt, ui_from_mgmt, ui_to_net, ui_from_net);
        let net_task = net::run(net_to_mgmt, net_from_mgmt, net_to_ui, net_from_ui);

        let write_data = b"hello ui circular";
        tokio::select!(
            _ = ctl_app.net_first_circular_ping(write_data) => {},
            _ = mgmt_task => {},
            _ = net_task => {},
            _ = ui_task => {},
        );
    }
}
