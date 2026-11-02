## Install

**macOS**: open the .dmg for your Mac (`arm64` for Apple Silicon, `x86_64` for Intel) and drag nordpil to Applications. The app isn't notarized, so macOS says it is damaged until you run this once:

```sh
xattr -dr com.apple.quarantine /Applications/nordpil.app
```

**Linux** (`x86_64` or `aarch64`):

- Debian or Ubuntu: `sudo apt install ./nordpil-@VERSION@-linux-<arch>.deb`
- AppImage: `chmod +x nordpil-@VERSION@-linux-<arch>.AppImage`, then run it.
- Anything else: unpack the .tar.gz and run `./install.sh`, or `./install.sh --user` to install into ~/.local without sudo.
