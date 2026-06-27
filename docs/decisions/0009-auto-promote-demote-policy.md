# 0009. Auto promote/demote policy for P2P relay

## Status

Proposed

## Context

P-3 must promote and demote relays automatically from perf signals, replacing P-2's manual
trigger. The catch: a viewer only downloads, so its upload capacity, the point of a relay,
cannot be measured passively. A receive-only peer's outbound bitrate reads near zero, so
trusting it pollutes the decision.

## Decision

- Judge on signals accurate for the node's current role: stability and link quality (RTT, loss)
  from passive perf reports; upload only by measurement, never assumption.
- Measure upload in two phases: an on-demand active probe before promotion, then the real
  outbound estimate plus leaf playback health after, demoting fast if it cannot sustain.
- Hysteresis: windowed averages, a promote/demote threshold band, and a cooldown before reversing.
- Demote on the relay's own signals and leaf feedback, not inference.

P-3 keeps one relay and one leaf per room (multi-leaf is P-4). Thresholds and message shapes
live in code.

## Alternatives considered

- **Trust the viewer's outbound bitrate.** Near zero on a receive-only peer.
- **Probe continuously, or on every connect.** Wastes uplink, hurts playback, stale by promotion.
- **Streak counts instead of a window.** Flaps at the boundary.

## Consequences

**Gain**: trustworthy signals; upload verified under load; band plus cooldown bounds flapping.

**Lose**: first promotion pays a probe round trip; the probe runs to the SFU, not the real
peer-to-peer path.

**Revisit**: P-4 multi-leaf changes the capacity math.
