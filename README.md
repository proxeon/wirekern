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

Lib default features are empty. CLI enables `vault-file` (`~/.postkit`, 0700/0600). No Graph connector in this scaffold — `postkit post threads` returns `unknown_site` until feature `threads` lands.

```bash
postkit apps set threads --client-id ID --client-secret SEC --redirect-uri https://localhost/callback
postkit auth threads --token THQVJ…   # needs a registered Publisher
postkit post threads --text "hi" --json
```

License: MIT OR Apache-2.0. Repository: [github.com/proxeon/postkit](https://github.com/proxeon/postkit).
