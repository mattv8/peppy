# Release verification keys

These public keys identify the initial stable and staging release-signing authorities.
Private keys and recovery backups are not stored in this repository.

| Channel | Purpose | File | GPG fingerprint |
| --- | --- | --- | --- |
| Stable | Release checksums | `stable-checksums.asc` | `22973A816783D4DEF91C64BBBDEFEAB01080A42D` |
| Stable | RPM packages | `stable-rpm.asc` | `204AC84AF920D3D1E694533635AC939311211C4E` |
| Staging | Release checksums | `staging-checksums.asc` | `AC5B51A417DC594073437FB52934DD9CE9C8D4A1` |
| Staging | RPM packages | `staging-rpm.asc` | `34DCB6D13A63D318D18B24245DB7D415BE925FC4` |

`stable-updater.pub` and `staging-updater.pub` contain Tauri's base64-encoded
Minisign public keys. Trusted builds embed the corresponding channel key, and
the publisher verifies updater payload signatures against it.

The channel keys are independent. Publishing the staging keys does not mean
the current staging branch has adopted the signed-release workflow.

## Trust and rotation

Verify these fingerprints through an independently trusted channel before
pinning them. A public key downloaded alongside an artifact does not by itself
authenticate its publisher. Neither a valid update signature nor a GPG signature
establishes Apple Developer ID or Windows Authenticode approval.

See [release provisioning](../README-updates.md) for signing policies and updater
key rotation. Do not replace a published updater key without a compatible,
authenticated update path for existing installations.
