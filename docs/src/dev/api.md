# API reference

The code's own documentation, built by `cargo doc` with private items, is published next to this book: **[rusty_painter](../api/rusty_painter/index.html)**.

To build it locally:

```bash
cargo doc --no-deps --document-private-items --open
```

CI builds it on every push with `RUSTDOCFLAGS="-D warnings"`, so a broken intra-doc link fails the build.
