#!/bin/bash

# XXX(RLB) This should ultimately be deleted.  It's just a test script that
# exercises certain functions of the device.

CTL=./target/debug/ctl
PORT=/dev/cu.usbserial-110
CTL=${BIN} --port ${PORT}

# Make a fresh build
cargo build

# Get info from all of the chips
#${CTL} mgmt info
#${CTL} ui info
#${CTL} net info

# Pings to verify all of the UART connections
${CTL} mgmt ping
${CTL} net ping
${CTL} ui ping
${CTL} circular-ping
${CTL} circular-ping --reverse

# Verify UI storage functionality
${CTL} ui get-version
${CTL} ui set-version $(openssl rand -hex 4 | sed -e "s/[^0-9]//g")
${CTL} ui get-version
${CTL} ui get-sframe-key
${CTL} ui set-sframe-key $(openssl rand -hex 16)
${CTL} ui get-sframe-key

# Verify NET storage functionality
${CTL} net add-wifi $(openssl rand -hex 4) $(openssl rand -hex 4)
${CTL} net add-wifi $(openssl rand -hex 4) $(openssl rand -hex 4)
${CTL} net get-wifi
${CTL} net clear-wifi
${CTL} net get-wifi
${CTL} net set-moq-url "https://moq.arpa/$(openssl rand -hex 4)" 
${CTL} net get-moq-url

