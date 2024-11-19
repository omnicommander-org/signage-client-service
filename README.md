# Signage Client

The client daemon for Digital Signage. It is designed to run continuously in the background of a client (such as a Raspberry Pi), where it pulls videos from the Digital Signage API and displays them through an MPV playlist.

## Getting Started

Before you can run the client, you need Rust installed: [Rust Getting Started Guide](https://www.rust-lang.org/learn/get-started).

Additionally, a config file at `~/.config/signage/signage.json` needs to be set up properly (see the "Configuration" section for more information).

To run the client, use `cargo run`. This will download and compile all of the dependencies, as well as compile and run the client.

## Configuration

There are two directories to be aware of:

1. `~/.config/signage` for the config file (`signage.json`)
2. `~/.local/share/signage` for data, video, and playlist files

## Installation

Move the release binary (*in progress*) to /usr/bin/signaged.

Add `@/usr/bin/signaged` to /home/pi/.config/lxsession/LXDE-pi/autostart.

### Example signage.json
```json
{
  "url": "https://ds-api.omnicommando.com",
  "id": "<client_id>",
  "username": "<username>",
  "password": "<password>"
}
```

## TODO:

- only download videos from whitelist
- move from tokio to blocking (we actually don't need tokio at all)
- release binaries
