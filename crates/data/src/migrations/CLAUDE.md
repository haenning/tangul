# Migrations of the settings files

Each settings file (`settings.json`, `databases/<id>.json`) carries a `"version"`:
its layout. `mod.rs` lists, per file, the steps between layouts; the current
version is the number of steps plus one. `upgrade.rs` runs the steps a file needs at
start and on "Upgrade".

- **Only from release to release.** A layout gets a version when it ships in a
  release, not per commit. Changes between two releases go into one step, written
  when the release is made: from the last released layout straight to the new one.
  Unreleased layouts (dev builds) get no step; such a file fails or is reverted by
  hand.
- **Only when keys move or change meaning.** A new key with a default needs no step:
  the upgrade adds missing keys at their default anyway.
- **Never change or drop a released step.** Users skip releases; a file of any
  released version must reach the current one.
- **A fixture per released version**, in `fixtures/<file>/v<N>.json`, as that release
  wrote it. The test upgrades each one and checks that it loads; add the release's
  fixture with its step.
- Rules that protect data (`ai.content_blocking`) fail closed: a step that can't
  carry a rule over must leave the file failing (hiding everything), never drop it.
