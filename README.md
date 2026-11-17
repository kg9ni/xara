# xara

a VARA-compatible modem for linux, freebsd, macOS and windows. tested against VARA HF 4.9.0 and FM 4.4.0.

## minimum config

set your call sign, HF bandwidth or FM mode, audio, and (if using a radio)
radio and ptt. do this with command line flags or a config file.

    xara --call YOURCALL --hf 2300 --audio alsa \
         --capture plughw:CARD=Device,DEV=0 \
         --playback plughw:CARD=Device,DEV=0

to transmit, add `--radio` and `--ptt` flags.

## other features

 - web UI for configuration and management
 - all the usual TCP ports (works with your favorite software)
 - digipeating (enable by call with `--digi N1CALL`)
 - `xara-monitor`: passive VARA HF monitor (with web UI)

## install

linux and freebsd use ALSA or Pulse. macOS uses CoreAudio. windows uses
WASAPI, the sound system built into Windows.

### linux

download the release tarball for your machine:

- `xara-0.1.4-x86_64-unknown-linux-gnu.tar.xz` for a normal PC
- `xara-0.1.4-aarch64-unknown-linux-gnu.tar.xz` for a 64-bit Pi
- `xara-0.1.4-armv7-unknown-linux-gnueabihf.tar.xz` for a 32-bit Pi

unpack it, then install the program and the service files:

    tar -xJf xara-0.1.4-x86_64-unknown-linux-gnu.tar.xz
    cd xara-0.1.4-x86_64-unknown-linux-gnu
    sudo ./dist/install.sh .

copy `dist/xara.conf.example` to `~/.config/xara/xara.conf`, fill in your
call sign and sound devices, and add:

    ui-port = 8080
    ui-control
    state-dir = /home/YOU/.local/state/xara

start it with `systemctl --user enable --now xara`, or run `xara --config ~/.config/xara/xara.conf`.
the page address is printed at startup. open that address in a browser.

### macOS

build from this folder (CoreAudio is the sound system; Pulse and ALSA are not used):

    cargo install --locked --path crates/xara

copy `dist/xara.conf.example` to `~/.config/xara/xara.conf` and set your call,
capture, and playback names (`xara --list-devices` prints them). to have it
start at login, edit `dist/launchd/xara.plist` (replace `YOU` with your login
name) and load it with launchctl. the web page address is in
`~/Library/Logs/xara.log`.

### windows

there is no installer file yet. build xara on the Windows PC. the Windows
build uses WASAPI for sound. USB CM108 keying and (tr)uSDX are not in this
version.

1. Install Rust from <https://rustup.rs>. The default choices are fine. Close
   the installer, then open a new PowerShell window.
2. Go to the xara folder and type this:

       cargo build --release --no-default-features

   `--no-default-features` leaves out the Linux sound libraries, which do not
   build on Windows. Success looks like `Finished release` and a program at
   `target\release\xara.exe`.
3. See the sound device names:

       .\target\release\xara.exe --list-devices

4. Start xara with your call sign, those names, and a COM port for the
   push-to-talk line. Example:

       .\target\release\xara.exe --call YOURCALL --hf 2300 --capture "Microphone" --playback "CABLE Input" --radio --ptt rts:COM3 --ui-port 8080 --ui-control

5. The page address is printed at startup. Open that address in a browser.

`--ptt rts:COM3` keys the radio with the RTS line on COM3. `dtr:COM3` uses DTR.
`rigctld` and `vox` work too. without `--radio`, playback has to be a virtual
cable: the name contains cable, virtual, voicemeeter, or null. Ctrl+C stops
xara and unkeys the radio. a second Ctrl+C, or closing the window, unkeys
before the program exits.

## building

install deps (debian/ubuntu):

    apt install libpulse-dev libasound2-dev

then install from here:

    cargo install --locked --git https://github.com/x0rro/xara xara

alternatively for a local setup you can do:

    cargo build --release
    cargo test

the systemd, udev and sysusers files and `dist/install.sh` are linux-only.

### reproducing a release

release tarballs are reproducible. rebuild one in an `ubuntu:22.04`
container on x86_64:

    # uncomment one of these:
    TARGET=x86_64-unknown-linux-gnu
    # TARGET=aarch64-unknown-linux-gnu
    # TARGET=armv7-unknown-linux-gnueabihf

    apt-get update && apt-get install -y --no-install-recommends ca-certificates git
    git clone -b vVERSION https://github.com/x0rro/xara && cd xara
    scripts/release_deps.sh $TARGET
    . ~/.cargo/env
    scripts/release_bins.sh $TARGET
    scripts/release_tarball.sh HEAD
    sha256sum target/dist/*.tar.xz

## options

    --config FILE           read key = value options from FILE

    --call CALL             your callsign (required)

    --audio BACKEND         pulse, alsa, coreaudio or wasapi; default pulse on linux/bsd, coreaudio on macOS, wasapi on windows
    --list-devices          list capture and playback devices for --audio
    --capture SOURCE        audio source
    --playback SINK         audio sink
    --tx-buffer-ms N        TX audio queue, raise on underruns (30-500, default 40)

    --hf 500|2300|2750      HF mode, bandwidth setting
    --fm narrow|wide        FM mode, narrow vs wide
    --retries N             initial connection attempts (default: 15)

    --radio                 enable radio mode
    --drive-db D            TX drive (<= 0 dB, default -5)
    --ptt [see section]     PTT has a lot of options

    --ui-port N             serve web UI on port N (default off)
    --ui-addr A             webui listen address (default 127.0.0.1)
    --ui-host NAME          accept as HTTP Host for UI (repeatable)
    --ui-token T            UI token (default: random)
    --ui-control            DANGER: remote token-holders can key radio
    --state-dir DIR         web UI settings storage (default: set by systemd)

    --addr A                KISS/command address (default: 127.0.0.1)
    --port N                command port (< 65535, default: 8300)
    --kiss-port N|off       KISS TCP port (default: 8100)
    --persistence P         KISS p-persistence

    --registered            enable registered speeds (must set build flag)

    --digi CALL             FM digipeat for CALL (default: off)
    --record PATH           record rx, tx and PTT to FLAC (or WAV if .wav)
    --log FILE              append UTC-timestamped output to FILE
    --quiet                 only output warnings and errors
    --version and --help    guess

## PTT settings

we support the following PTT methods:

 - rts and dtr on a serial port
 - cm108/cm119 HID GPIO
 - hamlib rigctld CAT
 - vox (not recommended)

you can adjust the following PTT settings as well:

 - PTT lead time
 - PTT tail time
 - max PTT

summary of settings:

    --ptt vox                   use VOX (just don't bother)
    --ptt rts:DEV               key serial port on rts, hold dtr low
    --ptt dtr:DEV               key serial port on dtr, hold rts low

on Windows, DEV is a COM port such as COM3 (`rts:COM3`). CM108 keying and
(tr)uSDX are not available in this Windows version.
    --ptt cm108:DEV[:GPIO]      cm108/cm119 HID GPIO (default GPIO: 3)
    --ptt cm108:VID:PID[:GPIO]  cm108/cm119 by hex USB id (stable across boots)
    --ptt rigctld[:HOST:PORT]   hamlib rigctld CAT on HOST:PORT

    --ptt-lead-ms N             PTT held for N ms before xmit (default: 0)
    --ptt-tail-ms N             PTT held for N ms after xmit (default: 100)
    --ptt-max-s N               release PTT after N sec max (1-120, default: 30)

## xara-monitor

`xara-monitor` listens to VARA HF traffic between other stations and never
transmits. it is considerably more demanding than `xara` and probably won't run
on a pi.

    xara-monitor --audio alsa --capture plughw:CARD=Device,DEV=0

options:

    --wav FILE              load from a wav file
    --stdin RATE            s16le mono at RATE Hz

    --audio BACKEND         pulse, alsa, coreaudio or wasapi; default coreaudio on macOS, wasapi on windows, else pulse
    --capture SOURCE        audio source

    --hf 500|2300|2750      HF bandwidth (default 2300)

    --call CALL             known call (repeatable)
    --calls FILE            list of known calls, one per line
    --no-blind              only attempt to resolve known calls

    --ui-port N             read-only web UI on port N (default: off)
    --ui-addr ADDR          web UI listen addr (default: 127.0.0.1)
    --ui-host NAME          accept as HTTP Host for UI (repeatable)

    --key-idle MIN          max target idle time (default: 30, 0 disables)
    --dump                  dump entire payloads
    --verbose               report unconfirmed blind detections
    --json                  print newline-delimited JSON records
    --log FILE              append UTC-timestamped output to FILE
    --version and --help    guess

## registered mode

compile with the `registered` feature for higher speeds if you have a
registered copy of VARA, then run with `xara --registered ...`.

## copyright

copyright (c) 2026 x0rro

released under MIT license, see LICENSE
