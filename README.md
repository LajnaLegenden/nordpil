# nordpil

A lightweight, native MongoDB GUI written in Rust with [egui](https://github.com/emilk/egui).
It covers the everyday parts of MongoDB Compass in a fraction of the memory: about 75 MB
when idle, against roughly 1 GB for Compass.

![Documents in a collection tab](docs/screenshots/main.png)

## Features

- **Connections**: saved connections, edited as a connection string or as separate fields
  kept in sync. Passwords are stored in the system keyring, or asked for on each connect
  when there is none. Connections can be colored and marked read-only, which blocks every
  write.
- **Browse**: a tree of databases and collections (with views, time series and capped
  collections marked). Create and drop databases and collections.
- **Go to collection**: Ctrl+K finds any collection by name, recent ones first, across
  every saved connection; picking one on a closed connection connects to it first.
- **Query**: filters in mongosh syntax (`{ age: { $gt: 30 }, _id: ObjectId('…') }`) with
  highlighting and autocomplete for operators, constructors, field names sampled from the
  collection and values from the current results. Brackets and quotes close themselves,
  and operators such as `$in` insert their brackets. Ctrl+L focuses the filter. Project,
  sort, collation, hint, skip, limit and max time. Query history and favorites per
  collection.
- **View**: list, JSON and table views with paging and counts.
- **Edit**: insert, edit, clone and delete documents. Edits save only the changed fields
  and refuse to overwrite a document that changed since it was loaded. Bulk update and
  delete show how many documents match first.
- **Indexes**: keys, properties, size and usage; create, drop, hide and unhide.
- **Import and export**: JSON, JSON lines and CSV, streamed with progress and cancel.
- **Settings**: dark, light or system theme, three text sizes, and whether Ctrl+K searches
  all saved connections or only open ones.

| Go to collection (Ctrl+K) | Values from the results |
| --- | --- |
| ![Go to collection](docs/screenshots/goto.png) | ![Value autocomplete](docs/screenshots/autocomplete.png) |
| **Table view** | **Editing a document** |
| ![Table view](docs/screenshots/table.png) | ![Edit dialog](docs/screenshots/edit-document.png) |
| **Indexes** | **Light theme** |
| ![Indexes](docs/screenshots/indexes.png) | ![Light theme](docs/screenshots/main-light.png) |

## Install

There are no release builds yet. Build from source with Rust 1.97 or newer.

On Linux, install it system-wide with a launcher entry and icon. This builds a release as
you, then copies the files to `/usr/local` with sudo:

```sh
just install          # or another prefix: just install /usr
just uninstall        # same prefix as install
```

On other platforms, or for the binary only:

```sh
cargo install --path crates/app
nordpil
```

On Linux, saving passwords needs a Secret Service keyring (GNOME Keyring, KeePassXC, …),
and the file picker uses the XDG desktop portal.

Saved connections, settings, query history and recent collections live in the
platform config folder (`~/.config/nordpil/` on Linux); logs in the platform state
folder (`~/.local/state/nordpil/logs/` on Linux).

## Development

The workspace has three crates:

- `crates/query`: parses mongosh shell syntax into BSON and back, highlighting and
  autocomplete. No UI or database code.
- `crates/backend`: the database work on a background tokio runtime, saved connections,
  keyring access, import and export.
- `crates/app`: the egui app.

Tasks use [just](https://github.com/casey/just):

```sh
just run          # start the app
just check        # formatting, lints and unit tests
just test-int     # integration tests against MongoDB in Docker
just screenshots  # render the UI to target/screenshots
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

The bundled [Geist](https://github.com/vercel/geist-font) fonts are under the SIL Open
Font License and the [Lucide](https://lucide.dev) icons under the ISC license; see
`crates/app/assets/fonts`.
