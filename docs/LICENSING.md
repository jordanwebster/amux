# Licensing

The whole repository is under the Functional Source License 1.1 with an
Apache 2.0 future licence, written `FSL-1.1-ALv2`. [LICENSE](../LICENSE) holds
the terms; this page answers the questions they raise.

## Which licence applies to a file

Every file is under `FSL-1.1-ALv2` — the multiplexer, the daemon, the
protocols, the transport, the terminal client, the libraries, the tooling and
the applications alike. The only exception is third-party code vendored into
the repository, which keeps its own licence (see below).

Commits made before 2026-09-22 were published with the core (everything
outside `apps/`) dual-licensed MIT or Apache-2.0. Those grants were made and
stand: anyone may keep using code from those commits under those terms. Every
later commit is under the FSL alone.

## What the FSL actually forbids

One thing: a Competing Use. That means making the software available to
others in a commercial product or service that substitutes for it, substitutes
for something else we offer using it, or offers substantially the same
functionality.

Everything else is permitted, and the licence names four cases explicitly:
internal use and access, non-commercial education, non-commercial research,
and professional services provided to someone who is themselves using the
software under these terms.

So you may read the source, build it, change it, run your own build on your
own machines and phone, and pass your changes on. You may not ship it as a
competing product.

## When code becomes Apache-2.0

Two years after it is made available, version by version.

The licence's own words are that the future licence is "effective on the
second anniversary of the date we make the Software available", and the FSL's
authors are explicit that publishing includes pushing a commit. This
repository is public, so **the clock starts when a commit is pushed**, not
when a release ships.

Three consequences worth being clear about:

- Nothing needs maintaining. There is no change date to nominate, no table to
  keep, and no renewal. A commit carries its own date, and that date is the
  whole mechanism.
- Components with different release schedules cost nothing extra. Each commit
  dates itself, so no two components need a shared calendar.
- Holding code back does not delay the clock, and shipping late does not start
  it. Pushed is published.

To find code you may use under Apache-2.0, check out any commit older than
two years. Tags are a convenience for finding those points, not part of how
the licence works.

## Contributing

A contribution is licensed to us under the FSL's terms, and converts to
Apache-2.0 on the same two-year clock as the version it lands in.

## Marketing and description

amux may not be described as open source: the FSL is not an OSI-approved
licence, and calling it open source would be wrong even though the source is
published. "Source-available" is the accurate phrase, and the two-year
conversion is worth stating alongside it, because it is the part that makes
the restriction temporary.

## What this does not cover

Third-party code vendored into the repository keeps its own licence, recorded
beside it. `crates/shot/assets/DejaVu-LICENSE.txt` is one such file.

Trademarks are not licensed. The FSL says so explicitly.
