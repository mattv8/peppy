# Local patch

This is SQLx 0.8.6 with one opt-in `PgListener::discard_on_drop` cleanup path.
It detaches and drops the listener connection instead of spawning the default
asynchronous `UNLISTEN` task. Remove this patch and the matching Cargo override
when upstream supports this behavior.

## Documentation-only scan compatibility change

`src/options/mod.rs` has two doc-comment lines changed in the
`ssl_client_key_from_pem` documentation: the prose and the doctest example no
longer spell a complete PEM private-key delimiter block, so the source scan does
not treat documentation as a secret. No code, behavior, or dependency version
changed. The doctest still passes its byte string to the same function. The
upstream registry checksum in `upstream-audit.lock` is intentionally unchanged;
it describes the registry package, not this local file.
