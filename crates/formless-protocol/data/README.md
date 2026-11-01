# Public Suffix List snapshot

`public_suffix_list.dat` is the complete ICANN and PRIVATE Public Suffix List,
downloaded from the canonical source
<https://publicsuffix.org/list/public_suffix_list.dat> on 2026-11-01.

- Upstream version: `2026-10-31_14-17-28_UTC`
- Upstream commit: `73f8cc701c084fe07d888ec552b2f0b53281a674`
- SHA-256: `617d1b276660e5a01209d7b58159450e400cf636634a7a9c2cfd50a1c6675d01`
- License: Mozilla Public License 2.0, preserved in the data header and
  `LICENSE-MPL-2.0` (from the upstream publicsuffix/list repository).

This is a pinned build input. Approval rendering never downloads a list.
To update, download from the canonical URL, preserve the license, update these
receipts from the downloaded bytes, and run the protocol rule/exception/private
suffix tests. See <https://publicsuffix.org/list/> and the upstream format:
<https://github.com/publicsuffix/list/wiki/Format>.

The matcher implements the full list's exact, wildcard and exception rules,
including PRIVATE entries. It uses IDNA ASCII normalization for rules and hosts.
Unknown single-label suffixes use the PSL's prevailing `*` rule conservatively.
This classification warns about broad authority; it does not identify the owner
or safety of a destination.
