//! Battery measurement abstraction and EV17 ADC voltage conversion.

/// Supplies periodic battery percentages. The implementation controls the sampling interval.
#[allow(async_fn_in_trait)]
pub trait BatteryMonitor {
    async fn next_level(&mut self) -> u8;
}

/// Boards without a battery monitor never produce a report.
impl BatteryMonitor for () {
    async fn next_level(&mut self) -> u8 {
        core::future::pending().await
    }
}

/// Convert PB0 millivolts directly to a rounded, clamped percentage.
/// With the 10k/15k divider, 2.22 V = 0%, 2.52 V = 100%.
pub fn percentage(millivolts: u32) -> u8 {
    ((millivolts.clamp(2220, 2520) - 2220 + 1) / 3) as u8
}

/// Convert 12-bit ADC readings to PB0 millivolts using factory VREFINT calibration
/// (measured at VDDA = 3.3 V), without scaling back to battery voltage.
pub fn millivolts(sample: u16, vref: u16, vref_cal: u16) -> Option<u32> {
    if vref == 0 {
        return None;
    }
    let numerator = u64::from(sample) * 3300 * u64::from(vref_cal);
    let denominator = u64::from(vref) * 4095;
    Some(((numerator + denominator / 2) / denominator) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voltage_range_and_clamping() {
        for (mv, expected) in [
            (0, 0),
            (2219, 0),
            (2220, 0),
            (2221, 0),
            (2222, 1),
            (2370, 50),
            (2520, 100),
            (3300, 100),
        ] {
            assert_eq!(percentage(mv), expected);
        }
    }

    #[test]
    fn pin_voltage_and_supply_compensation() {
        // Approximately 2.37 V at PB0.
        let nominal = millivolts(2941, 1500, 1500).unwrap();
        assert!((2369..=2371).contains(&nominal));
        // At lower VDDA both the battery and VREFINT ADC counts increase.
        let lower_supply = millivolts(3235, 1650, 1500).unwrap();
        assert!((2369..=2371).contains(&lower_supply));
        assert_eq!(millivolts(2941, 0, 1500), None);
    }

    #[tokio::test]
    async fn reports_share_ui_uart_with_tunneled_commands() {
        use crate::mgmt::{NetResetPins, UiResetPins};
        use crate::shared::mocks::{MockAsyncDelay, MockPin, async_async_channel, mock_led_pins};
        use crate::shared::{CtlToMgmt, CtlToUi, MgmtToCtl, NoOpBoard, ReadTlv, Tlv, WriteTlv};

        // A channel-backed monitor lets the test trigger each sample independently.
        struct Monitor(tokio::sync::mpsc::UnboundedReceiver<u8>);
        impl BatteryMonitor for Monitor {
            async fn next_level(&mut self) -> u8 {
                self.0.recv().await.unwrap()
            }
        }

        let ((mut ctl_rx, mut ctl_tx), (mgmt_rx, mgmt_tx)) = async_async_channel();
        let ((mut ui_rx, _ui_tx), (mgmt_ui_rx, mgmt_ui_tx)) = async_async_channel();
        let ((_net_rx, _net_tx), (mgmt_net_rx, mgmt_net_tx)) = async_async_channel();
        let (samples, receiver) = tokio::sync::mpsc::unbounded_channel();
        let task = crate::mgmt::run(
            mgmt_tx,
            mgmt_rx,
            mgmt_ui_tx,
            mgmt_ui_rx,
            mgmt_net_tx,
            mgmt_net_rx,
            mock_led_pins(),
            mock_led_pins(),
            UiResetPins::new(MockPin::new(), MockPin::new(), MockPin::new()),
            NetResetPins::new(MockPin::new(), MockPin::new()),
            MockAsyncDelay,
            NoOpBoard,
            Monitor(receiver),
        );

        tokio::select! {
            _ = task => unreachable!(),
            _ = async {
                samples.send(50).unwrap();
                let report: Tlv<CtlToUi> = ui_rx.read_tlv().await.unwrap().unwrap();
                assert_eq!(report.tlv_type, CtlToUi::BatteryLevel);
                assert_eq!(report.value.as_slice(), &[50]);

                // A CTL command still arrives intact on the same UART.
                let mut frame = std::vec::Vec::from(crate::shared::tlv::SYNC_WORD);
                frame.extend_from_slice(&Tlv::encode(CtlToUi::Ping, &[1, 2, 3]));
                ctl_tx.write_tlv(CtlToMgmt::ToUi, &frame).await.unwrap();
                let ping: Tlv<CtlToUi> = ui_rx.read_tlv().await.unwrap().unwrap();
                assert_eq!(ping.tlv_type, CtlToUi::Ping);
                assert_eq!(ping.value.as_slice(), &[1, 2, 3]);

                // Holding UI in reset suppresses reports; resuming allows the next one.
                ctl_tx.write_tlv(CtlToMgmt::SetPin, &[crate::Pin::UiRst as u8, 0]).await.unwrap();
                let ack: Tlv<MgmtToCtl> = ctl_rx.read_tlv().await.unwrap().unwrap();
                assert_eq!(ack.tlv_type, MgmtToCtl::Ack);
                samples.send(25).unwrap();
                assert!(tokio::time::timeout(
                    std::time::Duration::from_millis(10),
                    <_ as ReadTlv<CtlToUi>>::read_tlv(&mut ui_rx),
                ).await.is_err());
                ctl_tx.write_tlv(CtlToMgmt::SetPin, &[crate::Pin::UiRst as u8, 1]).await.unwrap();
                let _: Tlv<MgmtToCtl> = ctl_rx.read_tlv().await.unwrap().unwrap();
                samples.send(75).unwrap();
                let report: Tlv<CtlToUi> = ui_rx.read_tlv().await.unwrap().unwrap();
                assert_eq!(report.value.as_slice(), &[75]);
            } => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => panic!("battery report timed out"),
        }
    }
}
