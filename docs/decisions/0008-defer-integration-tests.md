# 0008. Defer integration tests

## Status

Accepted (2026-05-20). Amended (2026-09-11), see Revision.

## Context

Simulcast is implemented, and P2P relay is next. P2P relay will add more state transitions, signaling paths, and failure cases, so the code needs a test baseline before that work starts.

Unit tests are worth introducing now because they can cover local behavior without a large runtime setup. Integration tests would require more harness work, longer feedback loops, and unclear coverage value at the current project size.

## Decision

Introduce unit tests before P2P relay work, but defer integration tests for now.

Revisit integration tests when the project has enough cross-component behavior to make the setup cost worthwhile.

## Alternatives considered

- **Defer all test code.** Faster in the short term, but P2P relay is complex enough that adding tests only after it lands would make regressions harder to isolate.
- **Add integration tests now.** Better end-to-end confidence, but the harness cost is high relative to the current behavior surface.

## Consequences

**Gain**: local behavior gets test coverage before the next complex feature; integration-test setup does not block P2P relay development.

**Lose**: cross-component regressions still need manual testing or focused debugging until integration tests exist.

## Trigger to revisit

- P2P relay depends on multiple components whose behavior cannot be validated well with unit tests.
- Manual test effort starts to dominate feature work.
- A regression escapes that an integration test would have caught.

## Revision (2026-09-11)

Every trigger fired. Integration tests are now part of the suite.

The original reasoning was that scenarios written during active feature work would keep breaking, that the room lifecycle was not stable enough to script, and that the harness cost would not pay for itself at the project's size. Deferring until feature work settled and a baseline was needed.

Feature work is not finished, but what remains is performance work: room sharding, socket batching, and intra-room iteration, all of it queued behind feature development. None of it can be judged without a baseline to measure against, so the harness cost now unblocks the next milestone instead of competing with it. The room lifecycle, the WHIP contract, and the relay protocol have also settled enough to script without rewriting the scenarios every week.

The decisive argument is that the unit suite was already missing real failures. Two were live when the harness was built, and building it found both:

- the media socket advertised its bind address, `0.0.0.0`, which matches no ICE local candidate, so the server answered no STUN binding request and every connection stalled in `Checking`
- `Rid` and `Mid` serialized as byte arrays over the data channel, so viewers were told layer ids no client could read, and every inbound `set_layer` was dropped without a trace

Both are cross-component by construction. The types are correct, every unit that uses them is correct, and the failure only appears once a real client exchanges SDP over a real socket. No amount of unit testing reaches them.

Integration tests cover the viewer path, the room lifecycle, the control API contract, and the relay protocol as the server sees it. Auto layer downswitch stays out, because there is no way to make a congestion estimate fall reproducibly on loopback. The peer to peer link between two relay clients stays out as well, since the server never verifies it and browsers own that half.
