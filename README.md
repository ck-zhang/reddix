# Reddix Cookie Auth Fork

This is a fork of [ck-zhang/reddix](https://github.com/ck-zhang/reddix) that adds a personal-use cookie authentication mode. Thanks to [ck-zhang](https://github.com/ck-zhang) for creating Reddix.

The upstream Reddix app uses Reddit OAuth. This fork can instead read your existing browser Reddit cookies from environment variables and use them to load Reddit in the terminal. This is not an official or Reddit-approved authentication method. Abuse, unusual traffic, or detection by Reddit could result in rate limiting, session invalidation, or an account ban.

Reddix - Reddit, refined for the terminal.

![Reddix UI](docs/assets/reddix-ui-preview.png)

## Cookie Auth Quickstart

For the best experience, run Reddix in a terminal that supports the Kitty graphics protocol. [Ghostty](https://ghostty.org/) is a good choice on macOS. Apple Terminal can run the app, but it cannot display inline images.

1. Clone and build this fork:

```sh
git clone https://github.com/natekettles/reddix.git
cd reddix
cargo build --release
```

2. Open Reddit in Chrome while logged in.

3. Open Chrome DevTools:

```text
View -> Developer -> Developer Tools
```

4. Go to:

```text
Application -> Cookies -> https://www.reddit.com
```

5. Copy the cookie values named `reddit_session` and `token_v2`.

6. Export them in your shell:

```sh
export REDDIT_SESSION='your_reddit_session_cookie_value_here'
export TOKEN_V2='your_token_v2_cookie_value_here'
```

7. Optionally persist them by adding those same two lines to `~/.zshrc` or `~/.bashrc`, then reload your shell:

```sh
source ~/.zshrc
```

8. Test cookie auth without opening the TUI:

```sh
cargo run --example cookie_probe
```

You should see output like:

```text
children=1
```

9. Run Reddix:

```sh
cargo run --release
```

Or run the built binary directly:

```sh
./target/release/reddix
```

When `TOKEN_V2` is set, this fork automatically uses cookie auth and skips the OAuth setup flow. `REDDIT_SESSION` is optional for some reads, but setting both matches the browser session most reliably.

## Notes

- Cookie auth uses `https://www.reddit.com/` rather than the OAuth API host.
- Some Reddit responses may be rate limited. If the app shows a fetch error, wait a few minutes and retry.
- Inline image previews require a terminal that supports the Kitty graphics protocol, such as Ghostty or Kitty.
- Apple Terminal does not support Kitty graphics, so this fork disables those image escapes there.

## Features

- Image previews based on the kitty graphics protocol
- Video playback via [mpv](https://mpv.io)'s Kitty integration
- Gallery browsing with inline navigation controls
- Multi-account support
- Keyboard-first navigation
- Smart caching
- NSFW filter toggle

Core shortcuts: `j/k` move, `h/l` change panes, `m` guided menu, `o` action menu, `r` refresh, `s` sync subs, `u/d` vote, `q` quit.

## Upstream OAuth Setup

The original Reddix OAuth flow is still present. If you want to use official OAuth instead of cookie auth, unset `TOKEN_V2` and follow the upstream setup approach:

1. Apply for a Reddit app via the [Reddit support form](https://support.reddithelp.com/hc/en-us/requests/new?ticket_form_id=14868593862164&tf_14867328473236=api_request_type_enterprise).
2. Once approved, set the redirect URI to `http://127.0.0.1:65010/reddix/callback`.
3. Launch `reddix`, press `m`, and follow the guided menu for setup.

As of Nov 2025, Reddit blocked the old `reddit.com/prefs/apps` flow. Apply via the Reddit support form instead.
