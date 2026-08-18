# Issue #98 Connection Resource Bounds Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound hopd's client-to-daemon frame allocation, concurrent accepted connections, and stalled payload reads without adding an idle timeout or an accept-rate limiter.

**Architecture:** Keep the shared protocol codec's existing 256 MiB frame ceiling for daemon-to-client traffic, and add a narrower exported client-to-daemon ceiling that hopd checks after decoding the prefix but before allocating. Gate the accept call with an owned Tokio semaphore permit held for the entire connection task, and time only the payload read after a complete prefix has arrived. These three controls compose without changing the wire shape or adding a second connection lifecycle.

**Tech Stack:** Rust 2024, Tokio Unix sockets/semaphore/time, hop-protocol framing, Cargo workspace tests.

**Spec:** GitHub issue #98 and its full comments, especially the maintainer decision dated 2026-08-10 (`gh issue view 98 -R pedrosousa13/hop --json title,body,comments`).

## Global Constraints

- `MAX_INBOUND_FRAME_BYTES` is exactly `65_536` bytes and applies only to client-to-daemon payloads, before `connection.rs` allocates a payload buffer.
- `MAX_FRAME_BYTES` remains exactly `268_435_456` bytes and remains the shared/outbound frame ceiling; do not narrow daemon-to-client results frames.
- Concurrent connections are capped at exactly `64`. Acquire a permit before `listener.accept()` and hold the owned permit until that connection's spawned task ends, so the 65th connection waits in the listener backlog.
- Per-connection memory is the arithmetic already implied by enforced bounds, not a fifth knob: at most one 64 KiB inbound payload buffer plus the retained set already capped at `MAX_ITEMS_PER_RESULTS_FRAME = 1_000`. Across 64 connections, the inbound buffers total at most 4 MiB and retained sets total at most 64,000 bounded items.
- The payload completion timeout is exactly 10 seconds, beginning only after a full 4-byte length prefix is accepted. There is no timeout while an admitted connection is idle between frames.
- Add no token bucket or other accept-rate limiter. Preserve the existing 50 ms sleep after accept errors as a hot-spin floor and document that the connection cap is the chosen backpressure.
- Describe every new bound as same-uid robustness against buggy or runaway local clients, not a security boundary against a hostile peer.
- Preserve existing error behavior: an over-limit inbound prefix returns `ErrorCode::FrameTooLarge` without reading the payload; a timed-out payload read is an I/O failure that closes/logs through the existing connection-error path.
- Do not change `API_VERSION`: the wire representation is unchanged.
- Add no dependency; Tokio's existing `sync` and `time` features provide the semaphore and timeout.
- Follow strict red-green-refactor. Each new behavioral test must be observed failing for the expected missing-bound reason before production code is written.

---

### Task 1: Enforce and document all connection resource bounds

**Files:**
- Modify: `crates/hop-protocol/src/limits.rs`
- Modify: `crates/hopd/src/connection.rs`
- Modify: `crates/hopd/src/server.rs`
- Modify: `crates/hopd/tests/socket.rs`
- Modify: `docs/security/2026-08-02-m2-socket-boundary-threat-model.md`
- Commit: `docs/superpowers/plans/2026-08-18-issue-98-connection-resource-bounds.md`

**Interfaces:**
- Consumes: `hop_protocol::framing::payload_len`, `ErrorCode::FrameTooLarge`, `MAX_FRAME_BYTES`, `MAX_ITEMS_PER_RESULTS_FRAME`, `tokio::sync::Semaphore`, and the existing `serve_with`/`handle_connection` lifecycle.
- Produces: exported `hop_protocol::limits::MAX_INBOUND_FRAME_BYTES: usize`, private hopd constants for `MAX_CONCURRENT_CONNECTIONS: usize = 64` and a 10-second payload timeout, and observable backpressure/timeout behavior over the real Unix socket.

- [ ] **Step 1: Write the failing real-socket test for the connection cap**

  In `crates/hopd/tests/socket.rs`, add a test that derives its expectation independently with a local literal `const EXPECTED_CONNECTION_LIMIT: usize = 64`. Spawn hopd, connect and complete `Hello` on 64 streams, then connect a 65th stream and send `Hello`. Give the 65th stream a short read timeout and assert that reading its response prefix times out while all 64 slots are occupied. Drop one admitted stream, give the 65th a normal bounded read timeout, and assert that it then receives `DaemonMsg::HelloAck { api_version: API_VERSION }`.

  The core observation must have this shape:

  ```rust
  const EXPECTED_CONNECTION_LIMIT: usize = 64;

  let mut admitted = Vec::with_capacity(EXPECTED_CONNECTION_LIMIT);
  for _ in 0..EXPECTED_CONNECTION_LIMIT {
      let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
      hello(&mut stream);
      admitted.push(stream);
  }

  let mut waiting = UnixStream::connect(&daemon.socket_path).unwrap();
  send(
      &mut waiting,
      &ClientMsg::Hello {
          api_version: API_VERSION,
      },
  );
  waiting
      .set_read_timeout(Some(Duration::from_millis(250)))
      .unwrap();
  let mut prefix = [0_u8; 4];
  let blocked = waiting.read_exact(&mut prefix).unwrap_err();
  assert!(matches!(
      blocked.kind(),
      std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
  ));

  drop(admitted.pop());
  waiting.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
  assert_eq!(
      recv(&mut waiting),
      DaemonMsg::HelloAck {
          api_version: API_VERSION,
      }
  );
  ```

- [ ] **Step 2: Run the connection-cap test and verify RED**

  Run:

  ```bash
  cargo test -p hopd --test socket a_65th_connection_waits_until_one_of_64_slots_is_released -- --exact --nocapture
  ```

  Expected before implementation: FAIL because the 65th connection is handled immediately and returns a `HelloAck` instead of timing out.

- [ ] **Step 3: Implement semaphore backpressure and verify GREEN**

  In `crates/hopd/src/server.rs`, add a private `MAX_CONCURRENT_CONNECTIONS: usize = 64`, create one `Arc<Semaphore>` beside the listener, acquire an owned permit before every `accept`, and move the permit into the connection task. Bind it to a deliberately named variable for the entire task so it cannot be dropped before `handle_connection` finishes.

  The accept-loop structure must remain one loop and follow this ownership shape:

  ```rust
  let connection_slots = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

  loop {
      let permit = Arc::clone(&connection_slots)
          .acquire_owned()
          .await
          .map_err(|_| io::Error::other("connection limiter closed"))?;

      match listener.accept().await {
          Ok((stream, _addr)) => {
              let source = source.clone();
              tokio::spawn(async move {
                  let _connection_slot = permit;
                  if let Err(err) = handle_connection(stream, source).await {
                      eprintln!("hopd: connection error: {err}");
                  }
              });
          }
          Err(err) => {
              drop(permit);
              eprintln!("hopd: accept error: {err}");
              tokio::time::sleep(std::time::Duration::from_millis(50)).await;
          }
      }
  }
  ```

  Update the adjacent prose to state that the permit is acquired before `accept`, the 65th peer waits, the 50 ms error sleep is not a rate policy, and this is same-uid robustness.

  Re-run the exact test from Step 2. Expected: PASS.

- [ ] **Step 4: Write failing boundary tests for the inbound frame ceiling**

  In `crates/hopd/tests/socket.rs`, derive the boundary independently with a local `const EXPECTED_MAX_INBOUND_FRAME_BYTES: usize = 65_536`. Rename the existing real-socket oversize test to `an_inbound_frame_one_byte_over_64_kib_is_refused_from_prefix_alone`, use `(EXPECTED_MAX_INBOUND_FRAME_BYTES as u32) + 1`, keep writing only the prefix, and set a bounded read timeout so the pre-change implementation fails promptly instead of hanging while it waits for the advertised payload.

  Add `an_inbound_frame_exactly_64_kib_is_read_then_refused_as_malformed`, which writes exactly `EXPECTED_MAX_INBOUND_FRAME_BYTES` bytes of deliberately malformed JSON and expects `ErrorCode::MalformedFrame`. This distinguishes “the exact ceiling is admitted and parsed” from “one byte over is refused from the prefix alone.” Build the payload from a literal byte repeated to the exact length; do not compute expected behavior with hopd's check.

- [ ] **Step 5: Run the inbound-bound tests and verify RED**

  Run:

  ```bash
  cargo test -p hopd --test socket inbound_frame -- --nocapture
  ```

  If the repository's exact test-name filter does not match both new names, run each exact test separately. Expected before implementation: the one-byte-over test times out because the existing 256 MiB gate accepts its prefix and waits for a payload; the exact-ceiling malformed payload test already reaches `MalformedFrame` and serves as the passing side of the boundary pair.

- [ ] **Step 6: Implement the inbound pre-allocation gate and verify GREEN**

  Add `pub const MAX_INBOUND_FRAME_BYTES: usize = 65_536` to `crates/hop-protocol/src/limits.rs`. In `crates/hopd/src/connection.rs`, retain `payload_len(prefix)` as the shared `u32` decode and 256 MiB safety gate. Immediately after it returns `len`, compare `len` with `MAX_INBOUND_FRAME_BYTES`. On an over-limit value, return the existing `ReadOutcome::Refused` with `ErrorCode::FrameTooLarge` and `ErrorDetail::FrameTooLarge { len }`. Only then allocate `vec![0_u8; len]`.

  Import `MAX_INBOUND_FRAME_BYTES` beside `MAX_ITEMS_PER_RESULTS_FRAME`; do not alter `framing::payload_len`, because hop-cli must still accept legitimate large daemon-to-client results frames.

  Re-run both boundary tests from Step 5. Expected: PASS.

- [ ] **Step 7: Write failing unit tests for payload-only timing**

  In `connection.rs`'s existing test module, write against the desired private interface `read_frame(&mut read_half, payload_timeout: Duration)` before changing production code. Use `tokio::net::UnixStream::pair()` and real async I/O for two tests:

  1. Write an in-cap length prefix but leave the peer open without completing the payload. Call `read_frame` with a short test duration and assert the returned `io::ErrorKind` is `TimedOut`.
  2. Start `read_frame` with the same short duration, wait longer than it before writing any prefix, then write a complete encoded `ClientMsg::Hello`. Assert it is decoded successfully. This proves the timer does not cover idle time between frames and begins only after the prefix.

  The timeout assertion must pattern-match the result so it does not require `ReadOutcome: Debug`:

  ```rust
  let result = read_frame(&mut read_half, Duration::from_millis(25)).await;
  let Err(err) = result else {
      panic!("an incomplete payload must time out");
  };
  assert_eq!(err.kind(), io::ErrorKind::TimedOut);
  ```

- [ ] **Step 8: Run the payload-timeout tests and verify RED**

  Run each new unit test exactly:

  ```bash
  cargo test -p hopd connection::tests::an_incomplete_payload_times_out_after_its_prefix -- --exact --nocapture
  cargo test -p hopd connection::tests::idle_time_before_a_prefix_has_no_read_timeout -- --exact --nocapture
  ```

  Expected before implementation: RED at compilation because `read_frame` does not yet accept the timeout argument. This is the missing interface the next step supplies; after the signature exists, the first test must fail as `TimedOut` until the payload read is wrapped, while the second pins the required absence of an idle timeout.

- [ ] **Step 9: Implement the 10-second payload timeout and verify GREEN**

  Change `read_frame` to accept `payload_timeout: Duration`, and pass it from `read_loop`. Add a private `const INBOUND_PAYLOAD_READ_TIMEOUT: Duration = Duration::from_secs(10)` in `connection.rs`, with production `read_loop` passing that exact value. Leave the prefix `read_exact` outside every timeout. Wrap only the payload `read_exact` in `tokio::time::timeout`; map elapsed time to `io::Error::new(io::ErrorKind::TimedOut, "client frame payload read timed out")`.

  Re-run both exact tests from Step 8, then run:

  ```bash
  cargo test -p hopd connection::tests --lib
  cargo test -p hopd --test socket
  ```

  Expected: PASS.

- [ ] **Step 10: Document the composed memory bound and threat-model decision**

  In `limits.rs`, document `MAX_INBOUND_FRAME_BYTES` as the client-to-daemon cap and keep `MAX_FRAME_BYTES`'s outbound role explicit. State the composition without adding a constant: one connection can hold one 64 KiB inbound payload plus a retained set of at most 1,000 bounded items; 64 admitted connections compose to at most 4 MiB of inbound payload buffers plus 64,000 retained bounded items.

  In `server.rs`, document that 64 owned permits enforce the connection part of that arithmetic and that the connection cap, not a token bucket, is the intentional accept backpressure. Keep the existing 50 ms accept-error sleep unchanged.

  In `docs/security/2026-08-02-m2-socket-boundary-threat-model.md`, amend both the “connection itself” entry-point bullet and T13. Record the 64 KiB pre-allocation cap, 64-connection semaphore, 10-second payload-only timeout, deliberate lack of idle timeout/rate limiter, and the same-uid trust boundary. Remove claims that these controls are still unmodelled, without rewriting historical issue context unrelated to #98.

- [ ] **Step 11: Run the complete landing-quality implementation checks**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo test --workspace
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo deny check
  git diff --check
  ```

  Expected: every command exits 0 with no warnings promoted by Clippy and no whitespace errors.

- [ ] **Step 12: Self-review and commit**

  Review the diff against all five maintainer-approved decisions and the Global Constraints. Confirm there is no new protocol version, no fifth memory constant, no accept-rate limiter, and no timeout around the prefix read. Confirm each new test was recorded as RED before its implementation and is now GREEN.

  Commit all task files, including this plan:

  ```bash
  git add docs/superpowers/plans/2026-08-18-issue-98-connection-resource-bounds.md \
    crates/hop-protocol/src/limits.rs \
    crates/hopd/src/connection.rs \
    crates/hopd/src/server.rs \
    crates/hopd/tests/socket.rs \
    docs/security/2026-08-02-m2-socket-boundary-threat-model.md
  git commit -m "fix(hopd): bound connection resources"
  ```
