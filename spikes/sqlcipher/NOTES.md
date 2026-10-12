# SQLCipher spike notes

Checks for `docs/spike-sqlcipher-results.md`: presage's SQLite store on
SQLCipher, sharing the database with app tables.

## Run

```sh
nix develop
cd spikes/sqlcipher
cargo run --release
```

Prints one `PASS`/`FAIL` line per check and exits non-zero on any failure.
Uses a temporary directory that is removed afterwards. Never contacts Signal
servers.

The first build compiles libsignal and BoringSSL, so it takes several
minutes.

`migrations/` holds a stand-in for Copiale's own tables.
