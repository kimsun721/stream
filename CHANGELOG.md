# [Unreleased]

# [0.1.1]

## Added

- `stream_requests_refused`, in `/metrics` and `/metrics.json`: requests answered `503` because a media loop's channel was full. The Grafana dashboard has a panel for it.

## Changed

- An HTTP handler awaits a media loop's reply instead of holding a tokio worker while the loop answers. See [ADR 0012](docs/decisions/0012-reply-over-oneshot.md).
- A request to a media loop whose channel is full gets `503` instead of waiting, so control traffic never holds back media.
- A stream key is `SK_<loop>_<random>`, and resolving one asks only the loop that holds its room. A key without a valid loop number gets `401` without any loop being asked. To a backend the key is still an opaque string. See [ADR 0013](docs/decisions/0013-stream-key-carries-its-loop.md).

## Fixed

- A stream key check whose media loop never answers gets `500`, not `401`, so a server fault no longer reads as a bad key.
- Uncommenting the config volume in `compose.yaml`, as the README said to, left the file invalid.

# [0.1.0]

First release. See the [release notes](https://github.com/kimsun721/stream/releases/tag/v0.1.0).

[Unreleased]: https://github.com/kimsun721/stream/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/kimsun721/stream/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/kimsun721/stream/releases/tag/v0.1.0
