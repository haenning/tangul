# Security

tangul holds database passwords, SSH keys and API keys, and lets AI tools read your
databases. If you find a way around that, please tell me privately first.

## Reporting a vulnerability

Use GitHub's private reporting: **[Report a vulnerability](https://github.com/haenning/tangul/security/advisories/new)**
(the *Security* tab, *Report a vulnerability*). Please don't open a public issue, pull
request or discussion for it.

Helpful to include: the tangul version, the database and version, what you did, what
happened, and what you expected. A minimal SQL statement or settings file is ideal.

What happens next:

- You get an answer within a week.
- Once confirmed, I fix it in the latest release and publish an advisory, crediting
  you unless you'd rather not be named.
- Please give me 90 days before disclosing it yourself; I'll tell you if a fix ships
  sooner.

tangul is maintained by one person. There is no bug bounty.

## Supported versions

Only the latest release gets security fixes.

## What counts

Anything that breaks a promise in the README's [Safe on production](README.md#safe-on-production)
or [Investigate with AI](README.md#investigate-with-ai) sections, for example:

- A statement that changes data runs for the assistant, an MCP client, a schedule or a
  board, or runs past a read-only user (the engine's gate, `crates/engine/src/gate.rs`).
- An AI tool sees rows it shouldn't: from a schema-only database, a database not
  offered to AI tools, or columns that content blocking redacts or hides.
- The MCP server answers without its token, to a request with an `Origin`, or on
  anything but `127.0.0.1`.
- A password or key ends up outside the Keychain (settings, history, bookmarks, chats,
  the audit log, logs).
- An SSH host key that changed or wasn't trusted is accepted.

Not a vulnerability on its own: what a database user's own grants allow (tangul
recommends a SELECT-only user for the assistant), what a model does with data you chose
to send it, and TLS certificates not being verified yet (see the [roadmap](ROADMAP.md)).
