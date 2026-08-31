# 0010. Stream key authentication for publish

## Status

Proposed

## Context

WHIP ingest carries a bearer token, so the SFU has to decide what that token is. Today the header
is parsed but never checked, `/offer` takes the client role from the request body, and room CRUD
is open.

Validation runs before a client exists, since the client is what a successful request produces.
The credential has to belong to something that predates the connection.

Integrators are external backends, so the scheme constrains their code as much as ours.

RFC 9725 leaves the token's syntax and semantics out of scope, names shared secrets stored in a
database alongside JWTs, and points at RTMP stream key distribution as the typical model.

## Decision

- A stream key is a random string with no structure, no signature, and no expiry. The SFU issues
  one per room at creation and stores only its hash.
- The key authorizes and routes. It answers "may this request publish" and "to which room", not
  "who is this". Identity stays with the backend.
- The SFU assigns the room id rather than taking one from the caller. The id is the room's stable
  identity and the key is a rotatable credential, so the two cannot be the same value.
- The key is shown once at creation. Only the hash is kept, so it can never be read back, and
  reissue is the only recovery path.
- Room CRUD and reissue are control plane, authenticated by a server API key read from the
  environment. It is a backend-to-SFU credential and never reaches a browser.
- The same stream key authorizes creating a WHIP session and deleting it.

The room lifecycle this assumes, including what a validated session does to room state, is
[ADR 0011](0011-room-lifecycle-and-go-live.md).

## Alternatives considered

- **Signed token (JWT).** Forces a claim schema and a shared secret on every integrator, and
  saves no round trip, since the backend already calls the SFU to create the room.
- **Room id in the publish URL.** Gives every streamer a different endpoint and lets callers pick
  ids. Routing by key keeps one endpoint for everyone, matching RTMP practice.
- **Key stored on the client.** Validation precedes client creation, so it cannot live there.

## Consequences

**Gain**: the integrator stores and forwards a string and nothing else; the SFU keeps no user
model, so identity never becomes state it has to shard or persist; no crypto dependency beyond a
hash; storing the hash keeps the key out of logs that print a room; a leak is contained by
reissue.

**Lose**: the SFU cannot attribute a broadcast to a person, so per-user policy and abuse handling
stay in the backend; routing by key needs a key-to-room index that belongs to no single room,
read once per publish attempt and kept consistent with create, reissue, and delete; a lost key
can only be replaced; rooms now outlive broadcasts, so a restart drops every key and breaks every
configured encoder.

**Trigger to revisit**: room persistence, once losing keys on restart stops being acceptable.
Either a local store in the SFU, or letting the backend recreate rooms with the key it already
holds.
