#!/bin/bash

# XXX(RLB) This should ultimately be deleted.  It's just a test script that
# exercises certain functions of the device.

CTL=./target/debug/ctl

# Make a fresh build
cargo build

# All the pings
${CTL} --port /dev/cu.usbserial-110 mgmt ping
${CTL} --port /dev/cu.usbserial-110 net ping
${CTL} --port /dev/cu.usbserial-110 ui ping
${CTL} --port /dev/cu.usbserial-110 circular-ping
${CTL} --port /dev/cu.usbserial-110 circular-ping --reverse

# Verify UI storage functionality
${CTL} --port /dev/cu.usbserial-110 ui get-version
${CTL} --port /dev/cu.usbserial-110 ui set-version $(openssl rand -hex 4 | sed -e "s/[^0-9]//g")
${CTL} --port /dev/cu.usbserial-110 ui get-version
${CTL} --port /dev/cu.usbserial-110 ui get-sframe-key
${CTL} --port /dev/cu.usbserial-110 ui set-sframe-key $(openssl rand -hex 16)
${CTL} --port /dev/cu.usbserial-110 ui get-sframe-key

# Verify NET storage functionality
${CTL} --port /dev/cu.usbserial-110 net add-wifi $(openssl rand -hex 4) $(openssl rand -hex 4)
${CTL} --port /dev/cu.usbserial-110 net add-wifi $(openssl rand -hex 4) $(openssl rand -hex 4)
${CTL} --port /dev/cu.usbserial-110 net get-wifi
${CTL} --port /dev/cu.usbserial-110 net clear-wifi
${CTL} --port /dev/cu.usbserial-110 net get-wifi
${CTL} --port /dev/cu.usbserial-110 net set-moq-url "https://moq.arpa/$(openssl rand -hex 4)" 
${CTL} --port /dev/cu.usbserial-110 net get-moq-url

