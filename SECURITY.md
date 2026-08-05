# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security vulnerabilities. Instead,
report them privately via GitHub's Security Advisories:

<https://github.com/atheerium/voxtype/security/advisories/new>

You can also email the maintainers directly (see the repository's
`AUTHORS`/commit history for contact details).

We aim to acknowledge reports within **48 hours** and to ship a fix as soon as
practically possible. Please include:

- The affected version(s)
- A description of the issue and its impact
- Steps to reproduce (if possible)
- Any suggested fix

## Security properties of voxtype

- **No remote code.** voxtype runs entirely locally; the only network call is
  the audio transcription request to the provider you configure.
- **No telemetry or analytics.**
- **Secrets stay local.** The Groq API key is read from your config file or
  environment and is only sent to the provider in the `Authorization` header.
- **Careful process handling.** The daemon verifies process identity via
  `/proc/<pid>/comm` before killing processes it considers orphaned, and
  cleans up its own state files.

## Scope

The Rust source, the `install.sh` installer, and the GitHub Actions workflows
are in scope. Third-party dependencies (tokio, reqwest, etc.) are governed by
their own security policies.

## Supported versions

| Version | Supported |
|---|---|
| Latest release | ✅ |
| Older releases | ⚠️ Best effort |

Security fixes are backported to the latest release only.
