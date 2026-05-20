# 0008. Defer integration tests

## Status

Accepted (2026-05-20)

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
