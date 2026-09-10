# Configuration coverage snapshots

Coverage includes exact catalog and image digests, so the platform-specific
wallet builds require separate snapshots:

- `v1alpha1/configuration-coverage.json`: Linux ARM64 (the existing path is retained).
- `v1alpha1/configuration-coverage-linux-amd64.json`: Linux AMD64.

Regenerate both snapshots and the typed schemas on either supported host:

```sh
CARGO_TARGET_DIR=target/check cargo run --locked -p proofstorm-core --example export_schemas
```

Review the generated diff before committing it. Do not edit digests by hand.

The coverage test checks both complete snapshots on every run, not only the
host's platform. It also verifies that `default_catalog` selects the correct
platform and that only the two platform-specific wallet entries differ.
