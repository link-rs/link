#![allow(unused_imports)]
#![allow(dead_code)]

mod tlv;

pub mod mgmt {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use crate::tlv::{self, ReadTlv, Tlv, WriteTlv};
    use async_channel::{Receiver, Sender};
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

    pub struct Runner {
        stop: Arc<AtomicBool>,
        read_task: tokio::task::JoinHandle<()>,
        handle_task: tokio::task::JoinHandle<()>,
    }

    impl Runner {
        pub async fn start<W, R>(to_ctl: W, mut from_ctl: R)
        where
            W: Write + Send + 'static,
            R: Read + Send + 'static,
        {
            println!("Runner::start");
            const MAX_QUEUE_DEPTH: usize = 32;

            let (sender, receiver) = async_channel::bounded::<Event>(MAX_QUEUE_DEPTH);

            let mut app = App::default();
            let mut env = EnvironmentInstance::new(to_ctl);

            // Read thread
            let read_task = async move {
                println!("read task start");

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
                println!("handle task start");
                while let Ok(tlv) = receiver.recv().await {
                    // TODO convert TLVs to events
                    app.handle(tlv, &mut env).await;
                }
            };

            futures::join!(read_task, handle_task);
        }

        pub async fn stop(self) {
            self.stop.store(true, Ordering::SeqCst);
            self.read_task.await.unwrap();
            self.handle_task.await.unwrap();
        }
    }
}

pub mod ctl {
    use std::io::{Read, Write};

    pub struct App<R, W> {
        to_mgmt: W,
        from_mgmt: R,
    }

    impl<R, W> App<R, W> {
        pub fn new(to_mgmt: W, from_mgmt: R) -> Self {
            Self { to_mgmt, from_mgmt }
        }

        pub fn send_ping(&mut self, data: &[u8])
        where
            W: Write,
            R: Read,
        {
            println!("CTL write");
            self.to_mgmt.write(&[data.len() as u8]).unwrap();
            self.to_mgmt.write(data).unwrap();

            println!("CTL read");
            let mut len_buffer = [0u8; 1];
            while self.from_mgmt.read(&mut len_buffer).unwrap() == 0 {}

            let n: usize = len_buffer[0].into();
            let mut read_buffer = [0u8; 5];
            self.from_mgmt.read_exact(&mut read_buffer[..n]).unwrap();

            assert_eq!(data, &read_buffer);
            println!("CTL ok");
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use virtual_serialport::VirtualPort;

    fn virtual_port_pair() -> (VirtualPort, VirtualPort) {
        const SERIAL_BAUD_RATE: u32 = 9600;
        const SERIAL_BUFFER_CAPACITY: u32 = 1024;
        VirtualPort::pair(SERIAL_BAUD_RATE, SERIAL_BUFFER_CAPACITY).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn mgmt_ctl() {
        println!("mgmt_ctl start");
        let (ctl_side, mgmt_side) = virtual_port_pair();

        let mut ctl_app = ctl::App::new(ctl_side.clone(), ctl_side);
        let mgmt_runner = mgmt::Runner::start(mgmt_side.clone(), mgmt_side).await;

        let ctl_task = tokio::spawn(async move {
            let write_data = b"hello";
            ctl_app.send_ping(write_data);
        });

        ctl_task.await;
        mgmt_runner.stop().await;
    }
}
