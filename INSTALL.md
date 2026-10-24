# Installing tangul

tangul runs on macOS (Apple Silicon and Intel).

## From a release

1. Download `tangul-<version>-universal.dmg` from the
   [GitHub releases](https://github.com/haenning/tangul/releases).
2. Open it and drag tangul onto Applications.
3. The first start needs one extra step, because tangul isn't signed with an Apple
   Developer ID yet and macOS blocks apps it can't verify (again after each update you
   download). Your passwords' "Always Allow" in the Keychain stays across updates:
   every release is signed with the same certificate. Either:
   - open tangul once, then go to **System Settings → Privacy & Security**, scroll down
     and click **Open Anyway**, or
   - run this once in Terminal:

     ```sh
     xattr -dr com.apple.quarantine /Applications/tangul.app
     ```

## Updates

From then on tangul updates itself. At start and once a day it asks GitHub for the
latest release (one request, nothing sent but the request); a newer one shows as
**Update to x.y.z** in the title bar. **Install and restart** downloads it, checks that
the download and the app in it are signed with the same certificate as the running
app and that it is the version announced, swaps the two and starts the new one;
anything else leaves your app as it is. An update installed this way needs no extra step in Privacy & Security.

It can't update itself when started from the DMG or Downloads (move it to
Applications), when its folder isn't writable, or for a self-built app: then the menu
links to the release. `"updates": { "check": false }` in `settings.json` turns the
check off.

## From source

With the tools from [CONTRIBUTING.md](CONTRIBUTING.md#requirements) installed:

```sh
make install
```

This builds the app and copies it to `/Applications`. `make dmg` only builds
`target/bundle/tangul.app` and the DMG (`UNIVERSAL=1` for Apple Silicon + Intel).

**Keychain prompts after every build:** a self-built app is ad-hoc signed, and that
signature changes with each build. macOS then treats each install as a new app and
asks again for access to the saved passwords. Sign your builds with a stable local
certificate instead, once per Mac:

```sh
make signing-cert                                            # asks for your login password
echo 'TANGUL_SIGN_IDENTITY=tangul local signing' >> .env     # make reads .env, no quotes
make install
```

The first start after that asks once more; "Always Allow" then sticks across builds.

## Where your data lives

Everything stays on your Mac:

- `~/Library/Application Support/tangul/profiles.toml`: saved databases and users,
  without passwords.
- `~/Library/Application Support/tangul/settings.json`: settings (`⌘,` in the app).
- `~/Library/Application Support/tangul/local.db`: history, bookmarks, learned usage and
  a schema cache.
- The macOS Keychain (service `tangul`): database passwords and SSH secrets.

After installing a new build, macOS may ask once for access to the passwords saved by
an earlier build; choose **Always Allow**.
([#53](https://github.com/haenning/rusdb/issues/53) tracks making this survive
upgrades.)
