# Changelog

All notable, user-visible changes to [klappstuhl.me](https://klappstuhl.me) are
documented in this file. It is also rendered at
[klappstuhl.me/changelog](https://klappstuhl.me/changelog).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
as interpreted for a website.

Entries for 1.1.0 through 1.4.2 were backfilled from commit history when this
changelog was introduced in July 2026; changes between 1.0.0 (January 2025) and
1.1.0 (July 2026) predate it and are unrecorded.

## [Unreleased]

## [1.5.0] - 2026-07-14

### Added

- A public changelog at `/changelog`, rendered from this file with a colour-coded badge per category.
- The version the site is running now appears in the footer of every page, linking to the changelog.
- Profile pictures: your linked Discord avatar is shown on your account overview and on public profiles at `/user/<name>`. Accounts without a Discord link keep the default mark.

## [1.4.2] - 2026-07-14

### Changed

- Improved the password and API-key visibility toggles on the account pages.

## [1.4.1] - 2026-07-14

### Added

- Username changes: rename your account from the account settings, with a change history, a rename cooldown, a hold on the released name, and a live availability check on both signup and the rename dialog.

## [1.4.0] - 2026-07-14

### Added

- The account area is now a multi-page shell: profile, security, sessions, API keys, and a danger zone.
- Personal data export as JSON from the account area.
- Permanent account deletion; uploaded images you chose to keep and the audit trail survive ownerless.

## [1.3.3] - 2026-07-09

### Fixed

- Discord avatars are stored consistently after linking, and unlinking now asks for confirmation.

## [1.3.2] - 2026-07-09

### Fixed

- Logging out now also clears the Percy dashboard session.

## [1.3.1] - 2026-07-09

### Fixed

- The real Discord avatar is saved and served after linking a Discord account.

## [1.3.0] - 2026-07-08

### Added

- SVG chart rendering endpoints in the public API.
- Account introspection endpoint and color palette utilities in the public API.

### Changed

- Refreshed links across the site.

## [1.2.0] - 2026-07-07

### Added

- Paste hosting: upload and share text pastes through the site and the public API.

## [1.1.0] - 2026-07-07

### Added

- Per-guild gallery API keys.

### Security

- Privileged API scopes locked down: scope-limited keys now require an explicit grant.

## [1.0.0] - 2025-01-20

### Added

- Initial release: personal website, image hoster, and public JSON API.
