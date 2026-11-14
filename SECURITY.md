# Security policy

Mumbot is part of the Mumcord project. The [Mumcord security policy](https://github.com/Mumcord/mumcord/blob/main/SECURITY.md) applies to it in full: what to expect after a report, disclosure, and the rules for testing.

## Supported versions

Security fixes are made for the **latest release** and ship as a new patch release. Older releases do not get fixes; please update to the latest release.

## Reporting a vulnerability

**Do not report vulnerabilities in public issues, pull requests or discussions.**

Report privately through GitHub:

**<https://github.com/Mumcord/mumbot/security/advisories/new>**

Only you and the maintainers can read the report. If you cannot use this form, open a normal issue that only says you would like to report a security problem and asks for a private channel. Do not include any details in it.

A good report contains the Mumbot version and module (`webhook`, `peer`, `history`, ...), the Mumble server version, what an attacker can achieve and what they need for it (for example: a malicious server sending a crafted message, or a caller of the HTTP API), and steps to reproduce.

## What counts as a vulnerability

- A Mumble server, a chat user or an HTTP caller that can crash the bot, make it hang or use unbounded memory through crafted messages or requests.
- Access to a bot's HTTP API without a valid API key, or a way around the loopback-only default.
- A server certificate that is accepted although the configured pin rejects it, or the bot's client certificate or private key being exposed.
- Secrets such as API keys, passwords or private keys, or chat contents, ending up in logs.
- Unescaped text from the server or from callers reaching other users (for example HTML in chat).

Weaknesses of the Mumble protocol itself (for example its OCB2 voice crypto) are best reported to the [Mumble project](https://github.com/mumble-voip/mumble).
