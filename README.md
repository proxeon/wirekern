# postkit

Official-API publish kernel. You bring the app credentials. No calendar.

Rust library + CLI. HTTP later, same JSON.

```text
cargo add postkit
cargo install postkit-cli
```

```text
cargo test -p postkit
cargo run -p postkit-cli -- --help
```

Lib default features are empty. CLI enables `vault-file`, `threads`, and `bluesky` (`~/.postkit`, 0700/0600).

```bash
postkit auth threads --token THQVJ… --json
postkit post threads --text "hi" --json
postkit auth bluesky --account you.bsky.social --password 'xxxx-xxxx-xxxx-xxxx' --json
postkit post bluesky --text "hi" --json
postkit post --to threads,bluesky --text "hi" --json
```

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/postkit](https://github.com/proxeon/postkit).
