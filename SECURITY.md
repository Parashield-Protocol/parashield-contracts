# Security Policy


**Do not open a public GitHub issue for security vulnerabilities.**

Email: security@parashield.xyz  
PGP:   Available on request.

We aim to respond within 48 hours and will coordinate a disclosure timeline with you.

## Scope

- Oracle manipulation (false triggers, timestamp injection)
- Reentrancy or double-spend in claim settlement
- Admin key compromise / privilege escalation
- LP fund drainage via share arithmetic edge cases
- Governance proposal execution bypasses

## Out of Scope

- Stellar network-level issues
- Social engineering attacks
- Theoretical issues without a working PoC
- Issues in third-party dependencies not controllable by this codebase

## Bug Bounty

Critical vulnerabilities may be eligible for a bounty.
Details announced at launch.


## Supported Versions

| Version | Supported          |
|---------|--------------------|
| v2.x    | :white_check_mark: |
| v1.x    | :x:                |

## Reporting a Vulnerability