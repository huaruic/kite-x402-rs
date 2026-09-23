# Security Policy

This project sits on a payment path, so security reports are taken seriously
even though the wrapper itself holds no key material.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private vulnerability reporting:

> Repository → **Security** → **Report a vulnerability**

Please include what an attacker can achieve, the steps to reproduce, and the
affected version or commit. You can expect an acknowledgement within a few days.

## Scope

In scope, roughly in order of severity:

- Anything that lets a caller receive a paid response without being charged the
  configured amount — for example influencing the `paymentRequirements` sent to
  the facilitator, or reaching the upstream before `/verify` succeeds.
- Anything that causes a settlement for a request the upstream did not fulfil.
- Leaking `UPSTREAM_AUTH_VALUE`, or any inbound header, to the caller or to an
  unintended upstream.
- Request smuggling or header injection through the proxy path.

Out of scope:

- Vulnerabilities in the Kite facilitator, the Kite chain, or an upstream API
  you point this wrapper at. Report those to their maintainers.
- Denial of service through sheer request volume. The wrapper does no rate
  limiting by design; put a proxy in front of it.
- Findings that require an attacker to already control the deployment's
  environment variables.

## Design notes relevant to reports

- The wrapper never signs anything and never holds a private key. The payer
  signs an EIP-3009 authorization; the facilitator verifies, broadcasts and pays
  gas.
- The `accepted` block in a client's payload is never used to compute the
  charge. Requirements are built at startup from local configuration.
- Settlement happens strictly after the upstream returns a status below 400.
  This is covered by tests asserting exact facilitator call counts.
