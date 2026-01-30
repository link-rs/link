# Link

* `link` - Main logic crate.  Most of the logic should go here, so that we can
  make thorough, end-to-end test cases.
* `mgmt`, `net`, `ui` - Firmwares for the three chips.  Thin wrappers around the
  logic in `link`; basically just instantiates peripherals and an async
  environment and calls through to the `link` logic.
* `ctl` - Control program to run from a laptop connected to the device.
* `lib` - Tools that are helpful across multiple chips
* `web-ctl` - A WebSerial version of the CTL tool
* `web-link` - A web version of the Link device
* `vendor` - Dependencies that have been vendored so that they can be modified

See [QUICKSTART.md](QUICKSTART.md) for instructions on running the code.
