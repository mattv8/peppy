# Local patch

This is SQLx 0.8.6 with one opt-in `PgListener::discard_on_drop` cleanup path.
It detaches and drops the listener connection instead of spawning the default
asynchronous `UNLISTEN` task. Remove this patch and the matching Cargo override
when upstream supports this behavior.
