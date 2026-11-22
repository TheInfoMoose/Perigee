## Problem Description
`scripts/phased-step.sh` check fails a step when an existing tracker item changes after its snapshot, and nothing warns the writer. A Claude phase review closed four items at 2026-11-21T21:22:30Z while STEP 5BH ran, so a correct step failed its gate and was deferred. Suggested fix, in the phased-workflow skill and here: `status` reports the in-flight step, and review makes no writes until there is none.
Found during: Phase 5b review (Claude, read-only).

## Reproduction Steps
Start a step, `neb close` any existing item, then run check: '.tracker/ may only gain new pending items'.

## Fix Log & Agent Notes
