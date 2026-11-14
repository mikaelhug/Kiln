# Security policy

Kiln holds credentials that can publish images straight to production, so security reports get priority.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub: open the repository's **Security** tab and choose **Report a vulnerability**. Do not open a public issue.

Include what you found, how to reproduce it, and what an attacker gains. You can expect a first answer within a week. Fixes are released as a new version and credited in the changelog unless you prefer otherwise.

## Supported versions

Only the latest release receives security fixes.

## Scope

In scope:
- secrets or the registry credential reaching containers, logs or pull request builds that should not see them;
- bypassing the trust rules for tags and pull requests;
- authentication, session and webhook verification flaws in the server;
- escaping a build pod's isolation through anything Kiln configures.

Out of scope: weaknesses in a cluster or registry that Kiln's documentation tells you to configure differently, and denial of service by users who are allowed to sign in.
