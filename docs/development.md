# Development

```sh
cargo test -p insta360-stitch -p insta360-stitch-cli
cd bindings/node && npm ci && npm run build && npm test
cd bindings/python && uv run pytest
```

Some tests need real camera files. These tests read the files from the folder in
`INSTA360_FIXTURES`. If you do not set `INSTA360_FIXTURES`, the tests do not run.

The documentation and the code comments use Simplified Technical English (ASD-STE100).

## Contributing

To add support for a different camera or lens accessory, two files are necessary:

1. A photo from that camera or with that accessory.
2. The same photo, exported from Insta360 Studio.

These files let us measure the field of view and examine the stitch.
