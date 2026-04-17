# Audio capture via CTL

I would like to add a mode to the UI chip where it sends / receives audio
packets to / from CTL, and CTL either routes them to / from a speaker / mic, or
to / from WAV files.

This will require CTL to do Alaw and SFrame.  It should reuse the same logic
that the UI chip uses.

You should put basically all of the logic in `link::ctl` and `link::ui`.  For
microphones and speakers, link::ctl should have a trait that can be implemented
either by a platform audio library (e.g., `clap`) or by web audio APIs.

I have expressed the commands in `ctl` terms, but you should implement for
web-ctl as well.  Notes on how to do that below.

## Milestones

1. Update UI firmware 
2. CTL `capture live`
3. CTL `capture wav`
4. CTL `play wav`
5. CTL `play live`
6. web-ctl `capture live`
7. web-ctl `capture wav`
8. web-ctl `play wav`
9. web-ctl `play live`

## UI chip updates

```
# New command: ui audio-mode [get|set [ctl|net]]
# Defines which UART interface the UI chip sends audio frames to
ui audio-mode set ctl
```

When audio-out = ctl, the UI chip should send audio frames to the MGMT interface
instead of to NET.  Likewise, it should accept audio frames from the MGMT
interface and play them out.  Any audio frames received on the other interface
should be dropped (NET when mode=ctl; CTL when mode=net).  

Have the UI chip emit an AudioStart TLV when a button is first pressed and an
AudioEnd TLV when the button is released.  These TLVs should go to NET or CTL
depending on the mode.  NET can ignore them.  CTL will use them as described
below.

### Testing

Add a mock mic and speaker that inject / record samples.  For each audio mode:
Simulate a button press and verify that the correct sequence of
AudioStart/AudioFrame/AudioEnd TLVs are emitted on the appropriate UART
interface, and that the AudioFrames represent the injected samples.  Likewise,
inject AudioFrame TLVs on the correct and incorrect interfaces, and verify that
the correct ones get played out.

## Capture

```
# New command: audio capture [live|wav <basename>]
audio capture live
audio capture wav <basename>
```

The `audio capture` command should cause the `ctl` program to enter a mode that
does the following:

* Read the SFrame key from the UI chip
* Read the FromUi stream for audio frame packets. For each frame:
    * Decrypt the audio data with SFrame
    * Decode the audio data with Alaw
    * If `live`: Play the audio data out on the computer speaker
    * If `wav`: Add the audio to a buffer. 

(The decrypt/decode should be essentially the same as what the UI firmware does
when receiving audio.)

In `wav` mode, the system should save the buffered audio to a WAV file each time
it gets an AudioEnd TLV.  The filenames should be built from the basename by
appending a zero-prefixed number, e.g., `{basename}_003.wav`.

The WAV file processing logic should all live in `link::ctl`.  The `ctl` binary
should just expose a function that `link::ctl` can expose to save bytes to a
file.

### Testing

There should be a mock microphone that injects audio samples into UI.

In `live` mode, there should be a mock speaker that records audio samples
provided to it.  Verify that the samples provided in the mock microphone end up
being played at the mock speaker.

In `wav` mode, there should be a mock file saver.  Verify that the saved WAV
file represent the samples provided to the mock microphone.

## Play

CTL commands to initiate:

```
# New command: audio play [live|wav <filename>]
audio play live
audio play wav <basename>
```

The `audio play` command should cause the `ctl` program to enter a mode that
does the following:

* Read the SFrame key from the UI chip
* Read audio either from a microphone (`live` mode) or from a WAV file (`wav` mode)
* A-law encode and SFrame-encrypt the audio
* Send audio frames to the UI chip

(The encode/encrypt should be essentially the same as what the UI firmware does
when sending audio.)

In `live` mode, the space bar should act as a PTT button, so that `ctl` only
forwards media from the microphone when the space bar is held down.

In `wav` mode, the `ctl` app should show a progress bar for how much of the file
has been played out, then return to normal mode once playout is complete.

In either mode, `ctl` needs to pace the rate at which audio packets are sent.
The audio stream should be divided into packets of the standard length that UI
expects, and sent at the appropriate rate.  For example, if sending packets that
are 20ms long, `ctl` should only send every 20ms.

As with capture, WAV file processing should all be in `link::ctl`.  The `play
wav` logic should take bytes that the binary has already read from disk.

### Testing

There should be a mock speaker that records audio samples provided by UI.

In `live` mode, there should be a mock mircophone that provides audio samples
to CTL.  Verify that the samples provided in the mock microphone end up
being played at the mock speaker.

In `wav` mode, there should be a mock file loader.  Verify that the samples in
the loaded WAV correspond to those recorded by the mock speaker.

## Web-ctl UI

Add an audio-mode control to the UI block.

Make an Audio Test section, parallel to the MGMT/UI/NET sections.  As with
`ctl`, the SFrame key should be read out of the UI chip.

For capture: Toggle between inactive / play / WAV.  UI shows a dynamic list of
captured WAV files for playout or download.

For play: Toggle between inactive / mic / WAV.  Upload WAV, have a "play"
button.  PTT button for mic.

