## Problem Description
`scripts/phased-step.sh:393` - defer sets the task to pending but leaves its neb timer running, so the next `neb start` logs the idle time onto the deferred task (TASK-0111 got 396 minutes for a step that ran about 6; corrected by hand). Suggested fix: stop the timer before setting pending, in the phased-workflow skill's copy and this one.
Found during: Perigee cleanup after the STEP 5BH deferral.

## Reproduction Steps
1. `neb start` a step's task, then defer that step with `scripts/phased-step.sh defer`.
2. Some time later, `neb start` another task.
3. Observe: the deferred task is logged the whole gap.

## Fix Log & Agent Notes
