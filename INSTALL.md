# Installing rusdb

rusdb runs on macOS (Apple Silicon and Intel).

## From a release

1. Download `rusdb-<version>-universal.dmg` from the
   [GitHub releases](https://github.com/haenning/rusdb/releases).
2. Open it and drag rusdb onto Applications.
3. The first start needs one extra step, because rusdb isn't signed with an Apple
   Developer ID yet and macOS blocks apps it can't verify. Either:
   - open rusdb once, then go to **System Settings → Privacy & Security**, scroll down
     and click **Open Anyway**, or
   - run this once in Terminal:

     ```sh
     xattr -dr com.apple.quarantine /Applications/rusdb.app
     ```

## From source

With the tools from [CONTRIBUTING.md](CONTRIBUTING.md#requirements) installed:

```sh
make install
```

This builds the app and copies it to `/Applications`. `make dmg` only builds
`target/bundle/rusdb.app` and the DMG (`UNIVERSAL=1` for Apple Silicon + Intel).

## Where your data lives

Everything stays on your Mac:

- `~/Library/Application Support/rusdb/profiles.toml`: saved databases and users,
  without passwords.
- `~/Library/Application Support/rusdb/settings.json`: settings (`⌘,` in the app).
- `~/Library/Application Support/rusdb/local.db`: history, bookmarks, learned usage and
  a schema cache.
- The macOS Keychain (service `rusdb`): database passwords and SSH secrets.

After installing a new build, macOS may ask once for access to the passwords saved by
an earlier build; choose **Always Allow**.
([#53](https://github.com/haenning/rusdb/issues/53) tracks making this survive
upgrades.)
