## Problem Description
User Step 5R (2026-11-21): after follow-seat sessions ended, 5 `perigee-host capture-worker` processes (uid infomoose, up to 15 min old, one from a logged-out session) were still alive, all in wchan `anon_pipe_write`: the host stopped draining their stdout but kept the pipe open, so `WorkerCapturer` (crates/perigee-host/src/backends/worker.rs) was never dropped and never killed them. Live sessions also got only 2-4 frames in 5 s even with the mouse moving, i.e. the parent stops reading the worker after a few frames. Suspected: `crates/perigee-host/src/daemon.rs` run_capture_loop awaits `video_tx.send(...)` without racing `stop_rx`, backing up behind the QUIC pump (~1.8 MB raw frames), so the loop parks there and keeps the capturer Arc alive after the session stops. 5Q loopback with the direct WaylandShmCapturer ran 10 fps. Suggested fix: race the send against stop; stop/kill the worker explicitly on session stop; move capture to a blocking thread (TASK-0099); instrument capture vs encode vs send timing (TASK-0103).
Found during: User Step 5R (Phase 5a).

**Phase 5b review (2026-11-22): not fixed by STEP 5BF + 5BG. The spec was wrong.**
- The service runs as `perigee` without CAP_KILL; workers setuid to the session user. kill(2) from the service is therefore EPERM, for both `WorkerStopper::stop` and `Child::kill`, and both results are ignored.
- `stop_worker` then calls `child.wait()` while the `BufReader` is still alive (the `_` pattern does not drop it). The worker blocks writing to the open pipe and the parent waits forever. This also affects seat switches, not just session stop.
- Evidence from the 5R build: 5 "Starting capture worker", 0 "Stopped capture worker" (logged after wait), and the 5 workers still in anon_pipe_write 18 h later. That this deadlock caused every one of the five is inferred; the EPERM is certain from the uids and capability sets.
- The 5BF test uses a same-uid child, so it cannot see this.
- Same code: a stop during `poll_seat` is lost (no sticky stopped flag, so a worker can spawn after stop), and the SAFETY comment is false (stop_worker can reap between the stopper's swap and its kill).
- Suggested redesign, no new capability: stdin is a lifeline. The worker exits on stdin EOF; the parent stops it by closing stdin and dropping stdout before wait. A sticky stopped flag replaces the pid slot and the unsafe kill.

## Reproduction Steps
perigee connect <host> --for 5 against perigee-host --media follow-seat a few times, then `ps -eo pid,user,etime,args | grep capture-worker` and `cat /proc/<pid>/wchan`.

## Fix Log & Agent Notes
