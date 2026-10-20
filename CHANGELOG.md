# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-20

### Added

- `Message` (`Notify`, `Search`, `Response`) with a lenient parser
  (`Message::parse`, `FromStr`) and a formatter (`to_bytes`, `Display`).
- `Headers`: ordered, case-insensitive headers that remember quoting, so
  parsed messages round-trip byte for byte.
- `Device` and `Event`: typed views of announcements and search replies.
- `Discovery` / `discover()`: search and collect replies plus announcements,
  de-duplicated by `Host`.
- `Listener` / `watch()`: passive alive/offline events.
- `Announcer`: announce a device, answer matching searches, say goodbye.
- `default_interface()`: the IPv4 address used for multicast, found without
  sending packets.
- `tokio` feature (default). With `default-features = false` the crate is
  parsing and formatting only.

[Unreleased]: https://github.com/The1TrueJoe/sddp/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/The1TrueJoe/sddp/releases/tag/v0.1.0
