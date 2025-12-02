// The code in this crate must be no_std clean.  However, the test code uses std, specifically so
// that tokio can provide an async runtime.
#![cfg_attr(not(test), no_std)]

mod tlv;

pub mod mgmt {
    use crate::tlv::{CtlToMgmt, MgmtToCtl, ReadTlv, Tlv, WriteTlv};
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        fn to_ctl(&mut self) -> &mut impl Write;
    }

    pub struct EnvironmentInstance<W> {
        to_ctl: W,
    }

    impl<W> EnvironmentInstance<W> {
        fn new(to_ctl: W) -> Self {
            Self { to_ctl }
        }
    }

    impl<W> Environment for EnvironmentInstance<W>
    where
        W: Write,
    {
        fn to_ctl(&mut self) -> &mut impl Write {
            &mut self.to_ctl
        }
    }

    pub enum Event {
        Tlv(Tlv<CtlToMgmt>),
    }

    #[derive(Default)]
    pub struct App;

    impl App {
        async fn handle(&mut self, event: Event, env: &mut impl Environment) {
            match event {
                Event::Tlv(tlv) => self.handle_tlv(tlv, env).await,
            }
        }

        async fn handle_tlv(&mut self, tlv: Tlv<CtlToMgmt>, env: &mut impl Environment) {
            match tlv.tlv_type {
                CtlToMgmt::Ping => env
                    .to_ctl()
                    .write_tlv(MgmtToCtl::Pong, &tlv.value)
                    .await
                    .unwrap(),
            }
        }
    }

    pub async fn run<W, R>(to_ctl: W, mut from_ctl: R)
    where
        W: Write + 'static,
        R: Read + 'static,
    {
        const MAX_QUEUE_DEPTH: usize = 32;

        let (sender, receiver) = async_channel::bounded::<Event>(MAX_QUEUE_DEPTH);

        let mut app = App::default();
        let mut env = EnvironmentInstance::new(to_ctl);

        // Read thread
        let read_task = async move {
            loop {
                let tlv: Tlv<CtlToMgmt> = match from_ctl.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if sender.send(Event::Tlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        // Handle thread
        let handle_task = async move {
            while let Ok(tlv) = receiver.recv().await {
                // TODO convert TLVs to events
                app.handle(tlv, &mut env).await;
            }
        };

        futures::join!(read_task, handle_task);
    }
}

pub mod ui {
    use crate::tlv::{MgmtToUi, ReadTlv, Tlv, UiToMgmt, WriteTlv};
    use embedded_io_async::{Read, Write};

    pub trait Environment {
        fn to_mgmt(&mut self) -> &mut impl Write;
    }

    pub struct EnvironmentInstance<W> {
        to_mgmt: W,
    }

    impl<W> EnvironmentInstance<W> {
        fn new(to_mgmt: W) -> Self {
            Self { to_mgmt }
        }
    }

    impl<W> Environment for EnvironmentInstance<W>
    where
        W: Write,
    {
        fn to_mgmt(&mut self) -> &mut impl Write {
            &mut self.to_mgmt
        }
    }

    pub enum Event {
        MgmtTlv(Tlv<MgmtToUi>),
    }

    #[derive(Default)]
    pub struct App;

    impl App {
        async fn handle(&mut self, event: Event, env: &mut impl Environment) {
            match event {
                Event::MgmtTlv(tlv) => self.handle_mgmt_tlv(tlv, env).await,
            }
        }

        async fn handle_mgmt_tlv(&mut self, tlv: Tlv<MgmtToUi>, env: &mut impl Environment) {
            match tlv.tlv_type {
                MgmtToUi::Ping => env
                    .to_mgmt()
                    .write_tlv(UiToMgmt::Pong, &tlv.value)
                    .await
                    .unwrap(),
            }
        }
    }

    pub async fn run<W, R>(to_mgmt: W, mut from_mgmt: R)
    where
        W: Write + 'static,
        R: Read + 'static,
    {
        const MAX_QUEUE_DEPTH: usize = 32;

        let (sender, receiver) = async_channel::bounded::<Event>(MAX_QUEUE_DEPTH);

        let mut app = App::default();
        let mut env = EnvironmentInstance::new(to_mgmt);

        // Read thread
        let read_task = async move {
            loop {
                let tlv: Tlv<MgmtToUi> = match from_mgmt.read_tlv().await {
                    Ok(Some(tlv)) => tlv,
                    Ok(None) => return,  // Channel closed
                    Err(_err) => return, // IO error
                };
                if sender.send(Event::MgmtTlv(tlv)).await.is_err() {
                    break; // Receiver dropped, exit
                }
            }
        };

        // Handle thread
        let handle_task = async move {
            while let Ok(tlv) = receiver.recv().await {
                // TODO convert TLVs to events
                app.handle(tlv, &mut env).await;
            }
        };

        futures::join!(read_task, handle_task);
    }
}

pub mod ctl {
    use crate::tlv::{CtlToMgmt, MgmtToCtl, ReadTlv, Tlv, WriteTlv};
    use embedded_io_async::{Read, Write};

    pub struct App<R, W> {
        to_mgmt: W,
        from_mgmt: R,
    }

    impl<R, W> App<R, W> {
        pub fn new(to_mgmt: W, from_mgmt: R) -> Self {
            Self { to_mgmt, from_mgmt }
        }

        pub async fn send_ping(&mut self, data: &[u8])
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
    async fn mgmt_ctl() {
        let (ctl_to_mgmt, mgmt_from_ctl) = channel();
        let (mgmt_to_ctl, ctl_from_mgmt) = channel();

        let mut ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
        let mgmt_task = mgmt::run(mgmt_to_ctl, mgmt_from_ctl);

        let write_data = b"hello";
        tokio::select!(
            _ = ctl_app.send_ping(write_data) => {},
            _ = mgmt_task => {},
        );
    }
}
