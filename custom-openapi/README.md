# custom-openapi/

Hand-authored OpenAPI specs for APIs with no usable upstream spec yet.

One directory per service: `<service>/openapi/<file>.yaml`, wired into
`CUSTOM_SPEC_ENTRIES` in `src/spec.rs` via `include_str!`. A service named
here wins over the same name wired from the upstream specs.

Treat an entry as debt, not a home: once an upstream spec for the service
is usable, delete the override and this directory's copy. A maintainer-only
drift check flags a name wired on both sides as the signal to do that.
