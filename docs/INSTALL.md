# Installing Reddix

This document describes all supported ways to install Reddix.

## GitHub Releases

Download the latest [release](https://github.com/ck-zhang/reddix/releases/latest) for your platform. Extract the binary and place it in your `PATH`.

## Install script

Using the official install script (downloads the latest release and installs to a user directory):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/ck-zhang/reddix/releases/latest/download/reddix-installer.sh | sh
```

## Homebrew (macOS / Linux)

```sh
brew install reddix
```

## AUR (Arch Linux)

From source:

```sh
yay -S reddix
```

Pre-built binary:

```sh
yay -S reddix-bin
```

## From source

Requires [Rust](https://rustup.rs/) (stable).

```sh
git clone https://github.com/ck-zhang/reddix.git
cd reddix
cargo install --path .
```

Or run without installing:

```sh
cargo run
```

## Configuration

After installing, copy the example config and add your Reddit API credentials:

- **macOS / Linux**: `~/.config/reddix/config.yaml`
- **Windows**: `%APPDATA%/Reddix/config.yaml`

See [examples/config.yaml](examples/config.yaml) and the [Quickstart](../README.md#quickstart) in the main README for Reddit script setup.
