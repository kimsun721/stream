# 0013. A stream key carries its room's loop

## Status

Accepted (2026-10-15). Supersedes the stream key resolution in [ADR 0012](0012-reply-over-oneshot.md).

## Context

Since rooms spread across media loops, a room id carries its loop number, so every command about a room goes to one loop. A stream key carried nothing, so resolving one asked every loop.

It was the only HTTP request that fanned out. Its reply needed an `mpsc` where every other reply uses a `oneshot`, it answered `503` if any loop was full, and one wrong key cost a message on every loop.

[ADR 0010](0010-stream-key-authentication.md) calls a key "a random string with no structure". Its alternatives show what that ruled out: claims and a signature that every integrator would have to handle.

## Decision

- A stream key is `SK_<loop>_<random>`. The loop is its room's, read from the room id when the key is issued or reissued.
- Resolution goes to that loop alone, with a `oneshot` reply.
- A key without a loop number, or with one past the last loop, gets `401` without a loop being asked.
- The random part, and the hash taken over the whole key, are unchanged. The number routes and grants nothing: a client can write any number, and a wrong one reaches a loop that does not hold the key.

## Alternatives considered

- **Keep asking every loop.** One request routed unlike every other, and a wrong key costs every loop a message.
- **The room id inside the key, as a selector.** The loop would find the room directly instead of comparing hashes across its rooms, but a loop holds few rooms, and the key would grow by a whole room id.
- **An index from key hash to loop in the web layer.** Shared state that create, reissue and delete would have to keep in step with the loops.

## Consequences

**Gain**: every HTTP request reaches one loop and waits on a `oneshot`; a wrong key costs at most one message; resolution needs no `503` rule of its own.

**Lose**: a key is tied to where its room lives, as a room id already is. If the loop count changes across a restart once rooms persist, both need the same remapping, and a key is the harder one to replace, since it sits in an encoder's settings.

**Trigger to revisit**: rooms persisting across restarts, or rooms moving between loops.
