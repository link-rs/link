mod tlv;

pub mod mgmt {
    use crate::tlv::{self, ReadTlv, Tlv, WriteTlv};
    use embedded_io_async::{Read, Write};

    /// The Environment trait just allows a caller to get references to the objects we expect to
    /// have in an environment.  The main function of this trait is just to avoid having to
    /// propagate a bunch of generics on the `EnvironmentInstance` struct.
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
        Tlv(Tlv),
    }

    #[derive(Default)]
    pub struct App;

    impl App {
        async fn handle(&mut self, event: Event, env: &mut impl Environment) {
            match event {
                Event::Tlv(tlv) => self.handle_tlv(tlv, env).await,
            }
        }

        async fn handle_tlv(&mut self, tlv: Tlv, env: &mut impl Environment) {
            match tlv.tlv_type {
                tlv::Type::Ping => env
                    .to_ctl()
                    .write_tlv(tlv::Type::Pong, &tlv.value)
                    .await
                    .unwrap(),
                _ => {
                    // Silently ignore invalid types
                    // TODO log or validate upstream
                }
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
                let tlv = match from_ctl.read_tlv().await {
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

pub mod ctl {
    use crate::tlv::{self, ReadTlv, WriteTlv};
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
            self.to_mgmt.write_tlv(tlv::Type::Ping, data).await.unwrap();

            let tlv = match self.from_mgmt.read_tlv().await {
                Ok(Some(tlv)) => tlv,
                Ok(None) => return,  // Channel closed
                Err(_err) => return, // IO error
            };

            assert_eq!(tlv.tlv_type, tlv::Type::Pong);
            assert_eq!(&tlv.value, data);
        }
    }
}

#[cfg(test)]
mod test {
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
