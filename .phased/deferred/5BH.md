# STEP 5BH deferred

- Title: Capture Worker Sends Frames at the Wire Budget
- Task: TASK-0111
- Date: 2026-11-21T21:27:51Z
- Reason: orrery: Implementor unable to proceed within modification scope
- Attempt patch (local only): .git/phased/deferred/5BH.patch
- Restored: crates/perigee-host/src/host.rs
- Review (2026-11-22): not a scope or code fault. The attempt matched its spec and POST_GATE exited 0. A Claude phase review closed BUG-0009, TASK-0098, TASK-0101 and TASK-0102 mid-step, which failed the tracker check. Withdrawn; re-issue as one step after send timing exists (TASK-0103: the change raises wire bytes 1.78x).
