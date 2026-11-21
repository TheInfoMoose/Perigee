## Problem Description
`README.md:45`: the "Running the host" note still says "there is no rate limiting yet", but STEP 4BG added `HandshakeLimiter` (20 handshakes per IP per 60 s) and `MAX_OPEN_CONNECTIONS` (64) in `crates/perigee-net/src/endpoint.rs`. Suggested fix: say handshakes are rate limited per IP and capped, and keep the advice to bind to loopback or a VPN interface.
`README.md:79-91`: the loopback/VPN run-examples code block now sits under the `### Pairing (one copy)` heading (the 4BL spec placed pairing "right after" the manual exchange, ahead of that block). Suggested fix: move the run examples above the pairing subsection, or give them their own `### Running` heading.
Found during: STEP 4BL validation.

## Reproduction Steps
n/a (docs)

## Fix Log & Agent Notes
