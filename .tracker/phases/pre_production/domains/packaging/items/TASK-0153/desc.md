## Problem Description
Imported from roadmap step `STEP 10BB`: Fix perigee-capture-broker.socket DirectoryMode=0755 (BUG-0044) and add AF_NETLINK to perigee-host.service RestrictAddressFamilies (BUG-0046)
- Step ID: `STEP 10BB`
- Command: `/goal`
- Roadmap order: 2
- Target files:
  - `packaging/linux/perigee-capture-broker.socket`
  - `packaging/linux/perigee-host.service`

## Reproduction Steps
1. Inspect `packaging`.
2. Run tests to verify behavior.

## Fix Log & Agent Notes
- Target scope path: `packaging`
