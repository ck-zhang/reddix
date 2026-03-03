# Security

## Reporting a vulnerability

If you believe you have found a security vulnerability in Reddix, please report it responsibly:

1. **Do not** open a public GitHub issue for the vulnerability.
2. Send a description of the issue to the maintainers. You can reach the project owner via the contact options listed on their [GitHub profile](https://github.com/ck-zhang).
3. Include steps to reproduce, affected versions, and the impact of the issue if possible.
4. Allow a reasonable time for a fix before any public disclosure (we aim to respond within a few days and address critical issues as soon as possible).

## What to expect

- We will acknowledge receipt of your report and may ask for clarification.
- We will work on a fix and keep you updated when feasible.
- We will credit you for the discovery in the fix announcement (unless you prefer to remain anonymous).

## Scope

- Reddix stores Reddit OAuth tokens and configuration locally. Issues related to token handling, storage, or exposure are in scope.
- Reddit API credentials (client ID/secret) are configured by the user; ensure they are not committed or logged. Reports about accidental exposure in the codebase are welcome.

Thank you for helping keep Reddix and its users safe.
