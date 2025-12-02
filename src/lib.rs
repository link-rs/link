pub mod mgmt {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use heapless::mpmc::Queue;

    // TODO
    // * Instantiate Environment with an InMemoryEnvironment, probably still generic
    // * Define a TLV reader / writer
    //      * Bounded memory
    //      * Cancellable reader
    //      * Async?  At least on the read side?
    // * Define an event type
    // * Runner should:
    //      * Make an event queue
    //      * Make threads that feed the event queue from readers
    //      * Handle events from the queue

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

    #[derive(Default)]
    pub struct App;

    impl App {
        fn handle(&mut self, data: &[u8], env: &mut impl Environment) {
            env.to_ctl().write(&[data.len() as u8]).unwrap();
            env.to_ctl().write(data).unwrap();
        }
    }

    pub struct Runner {
        stop: Arc<AtomicBool>,
        read_task: JoinHandle<()>,
        handle_task: JoinHandle<()>,
    }

    impl Runner {
        pub fn start<W, R>(to_ctl: W, mut from_ctl: R) -> Self
        where
            W: Write + Send + 'static,
            R: Read + Send + 'static,
        {
            const MAX_QUEUE_DEPTH: usize = 32;

            let stop = Arc::new(AtomicBool::new(false));

            // Queue::new is deprecated, but the usage we have here should be OK.
            #[expect(deprecated)]
            let queue: Arc<Queue<Vec<u8>, MAX_QUEUE_DEPTH>> = Arc::new(Queue::new());

            let mut app = App::default();
            let mut env = EnvironmentInstance::new(to_ctl);

            // Read thread
            let stop_read = stop.clone();
            let read_queue = queue.clone();
            let read_task = std::thread::spawn(move || loop {
                if stop_read.load(Ordering::SeqCst) {
                    break;
                }

                let mut len_buffer = [0u8; 1];
                while from_ctl.read(&mut len_buffer).unwrap() == 0 {
                    if stop_read.load(Ordering::SeqCst) {
                        break;
                    }
                }

                let n: usize = len_buffer[0].into();
                let mut buffer = vec![0; n];
                from_ctl.read_exact(&mut buffer).unwrap();

                read_queue.enqueue(buffer).expect("Queue overflow");
            });

            // Handle thread
            let stop_handle = stop.clone();
            let handle_queue = queue.clone();
            let handle_task = std::thread::spawn(move || loop {
                if stop_handle.load(Ordering::SeqCst) {
                    break;
                }

                match handle_queue.dequeue() {
                    Some(data) => app.handle(&data, &mut env),
                    None => {}
                }
            });

            Self {
                stop,
                read_task,
                handle_task,
            }
        }

        pub fn stop(self) {
            self.stop.store(true, Ordering::SeqCst);
            self.read_task.join().unwrap();
            self.handle_task.join().unwrap();
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
            self.to_mgmt.write(&[data.len() as u8]).unwrap();
            self.to_mgmt.write(data).unwrap();

            let mut len_buffer = [0u8; 1];
            while self.from_mgmt.read(&mut len_buffer).unwrap() == 0 {}

            let n: usize = len_buffer[0].into();
            let mut read_buffer = [0u8; 5];
            self.from_mgmt.read_exact(&mut read_buffer[..n]).unwrap();

            assert_eq!(data, &read_buffer)
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

    #[test]
    fn mgmt_ctl() {
        let (ctl_side, mgmt_side) = virtual_port_pair();

        let mut ctl_app = ctl::App::new(ctl_side.clone(), ctl_side);
        let mgmt_app = mgmt::Runner::start(mgmt_side.clone(), mgmt_side);

        let write_data = b"hello";
        ctl_app.send_ping(write_data);

        mgmt_app.stop();
    }
}
