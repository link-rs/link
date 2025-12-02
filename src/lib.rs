mod mgmt {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::thread::JoinHandle;

    pub trait Environment {
        fn to_ctl(&mut self) -> &mut impl Write;
    }

    pub struct App;

    impl App {
        fn handle<W: Write>(data: &[u8], env: &mut impl Environment) {
            env.to_ctl().write(data).unwrap();
        }
    }

    pub struct AppRunner {
        stop: Arc<AtomicBool>,
        join_handle: JoinHandle<()>,
    }

    impl AppRunner {
        pub fn start<W, R>(mut to_ctl: W, mut from_ctl: R) -> Self
        where
            W: Write + Send + 'static,
            R: Read + Send + 'static,
        {
            let stop = Arc::new(AtomicBool::new(false));

            let stop_clone = stop.clone();
            let join_handle = std::thread::spawn(move || loop {
                if stop_clone.load(Ordering::SeqCst) {
                    break;
                }

                let mut len_buffer = [0u8; 1];
                while from_ctl.read(&mut len_buffer).unwrap() == 0 {
                    if stop_clone.load(Ordering::SeqCst) {
                        break;
                    }
                }

                let n: usize = len_buffer[0].into();
                let mut read_buffer = [0u8; 5];
                from_ctl.read_exact(&mut read_buffer[..n]).unwrap();

                to_ctl.write(&len_buffer).unwrap();
                to_ctl.write(&read_buffer[..n]).unwrap();
            });
            Self { stop, join_handle }
        }

        pub fn stop(self) {
            self.stop.store(true, Ordering::SeqCst);
            self.join_handle.join().unwrap();
        }
    }
}

mod ctl {
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
        let mgmt_app = mgmt::AppRunner::start(mgmt_side.clone(), mgmt_side);

        let write_data = b"hello";
        ctl_app.send_ping(write_data);

        mgmt_app.stop();
    }
}
