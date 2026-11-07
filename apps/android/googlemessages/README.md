# Google Messages link

This Go module builds the opt-in Android bridge for the Google Messages link.

## Pins

- libgm: `go.mau.fi/mautrix-gmessages v0.2610.1-0.20261103213626-d9140a5e807e`
- gomobile and gobind: `golang.org/x/mobile v0.0.0-20261011034528-197770444867`
- private patch SHA-256: `46fdf18564e8d1616f383c1a9a8d8f27506d0d6b22d48569153d3986edf6e326`

## Commands

```sh
./prepare-libgm.sh
go test -race ./...
go vet ./...
./build-aar.sh
```

`prepare-libgm.sh` copies the pinned upstream source into `build/third_party`,
checks and applies the private safety patch, and verifies the resulting tree.

Peppy's Go code is Apache-2.0. The private patch contains AGPL-3.0-or-later
libgm source excerpts; any binary linking libgm is AGPL-3.0-covered and is not
released until roadmap gate G2.
