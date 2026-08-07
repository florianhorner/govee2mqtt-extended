# Airtime A/B proof — LAN status-query retransmit bound

Date: 2026-08-07. Both sides measured with the identical harness: a real UDP
socket bound to `127.0.0.1:4003` (the Govee CMD port) counting datagrams that
`query_status` puts on the wire against a device that never answers. The
harness is committed on this branch as
`lan_api::tests::loopback_retransmit_bound_and_reply`; for the old side the
same harness was temporarily added to a throwaway worktree at `origin/main`
(commit `64942a0`) and never committed.

## A — old code (origin/main @ 64942a0)

Flat 350 ms retransmit inside a fixed 10 s deadline (`src/lan_api.rs:599-616`
on main):

```
$ cargo test airtime_proof   # harness panics deliberately to print the count
running 1 test
OLD_CODE_DATAGRAMS_SENT: 29
test result: FAILED. 0 passed; 1 failed; ... finished in 11.36s
```

**29 datagrams, caller held ~10 s.**

## B — new code (this branch)

`LanQueryPolicy` bounded schedule (3 attempts, 350→700→1400 ms doubling):

```
$ cargo test loopback_retransmit_bound_and_reply
running 1 test
test lan_api::tests::loopback_retransmit_bound_and_reply ... ok
test result: ok. 1 passed; 0 failed; ... finished in 5.83s
```

The test hard-asserts the counts — it fails if the wire sees anything else:

- silent device: **exactly 3 datagrams**, then `Err` (caller held ~2.45 s at
  default backoff; the test uses a 100 ms backoff for speed)
- device answering the 2nd attempt: **exactly 2 datagrams**, no retransmit
  after the reply
- device with an open circuit breaker (3 consecutive failed queries):
  **0 datagrams** on the gated periodic-poll path

## Result

| | datagrams to a silent device | caller hold time |
|---|---|---|
| old (`64942a0`) | 29 | ~10 s |
| new (this branch) | 3 | ~2.45 s |

**9.7× fewer packets per unresponsive device per poll.** With the circuit
breaker on top (threshold 3), a persistently dead device drops from
29 packets every 30 s cycle (~3,480/h) to 3 packets per 5–15 min cooldown
window (≤36/h), and 0 while the breaker is open.

Production baseline for comparison (measured on HA Green 2026-08-07, healthy
band): 47 Govee LAN packets / 35 s across 22 LAN devices via
`tcpdump -nn -i any -q "udp and (port 4001 or port 4002 or port 4003)"`.
Healthy-path behavior is unchanged: one request, one reply.
