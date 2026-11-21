## Problem Description
User Step 5R (2026-11-21): after follow-seat sessions ended, 5 `perigee-host capture-worker` processes (uid infomoose, up to 15 min old, one from a logged-out session) were still alive, all in wchan `anon_pipe_write`: the host stopped draining their stdout but kept the pipe open, so `WorkerCapturer` (crates/perigee-host/src/backends/worker.rs) was never dropped and never killed them. Live sessions also got only 2-4 frames in 5 s even with the mouse moving, i.e. the parent stops reading the worker after a few frames. Suspected: `crates/perigee-host/src/daemon.rs` run_capture_loop awaits `video_tx.send(...)` without racing `stop_rx`, backing up behind the QUIC pump (~1.8 MB raw frames), so the loop parks there and keeps the capturer Arc alive after the session stops. 5Q loopback with the direct WaylandShmCapturer ran 10 fps. Suggested fix: race the send against stop; stop/kill the worker explicitly on session stop; move capture to a blocking thread (TASK-0099); instrument capture vs encode vs send timing (TASK-0103).
Found during: User Step 5R (Phase 5a).

## Reproduction Steps
perigee connect <host> --for 5 against perigee-host --media follow-seat a few times, then `ps -eo pid,user,etime,args | grep capture-worker` and `cat /proc/<pid>/wchan`.

## Fix Log & Agent Notes
