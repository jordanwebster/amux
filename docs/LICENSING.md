# Licensing

Two licences, split by directory. [LICENSE](../LICENSE) states the split;
this page answers the questions it raises.

## Which licence applies to a file

Everything inside `apps/` is under the Functional Source License 1.1 with an
Apache 2.0 future licence, written `FSL-1.1-ALv2`. The iPhone app is at
`apps/apple/`. Everything else is dual MIT or Apache-2.0, at the recipient's
option.

There is no third case and no per-file exception. The path decides.

## What the FSL actually forbids

One thing: a Competing Use. That means making the software available to
others in a commercial product or service that substitutes for it, substitutes
for something else we offer using it, or offers substantially the same
functionality.

Everything else is permitted, and the licence names four cases explicitly:
internal use and access, non-commercial education, non-commercial research,
and professional services provided to someone who is themselves using the
software under these terms.

So you may read the app's source, build it, change it, run your own build on
your own phone, and pass your changes on. You may not ship it as a competing
product.

## When code becomes Apache-2.0

Two years after it is made available, version by version.

The licence's own words are that the future licence is "effective on the
second anniversary of the date we make the Software available", and the FSL's
authors are explicit that publishing includes pushing a commit. This
repository is public, so **the clock starts when a commit is pushed**, not
when an app ships.

Three consequences worth being clear about:

- Nothing needs maintaining. There is no change date to nominate, no table to
  keep, and no renewal. A commit carries its own date, and that date is the
  whole mechanism.
- Several applications with different release schedules cost nothing extra.
  Each commit dates itself, so no two apps need a shared calendar.
- Holding code back does not delay the clock, and shipping late does not start
  it. Pushed is published.

To find code you may use under Apache-2.0, check out any commit older than
two years. Tags are a convenience for finding those points, not part of how
the licence works.

## Why the core is dual MIT or Apache-2.0

It is the Rust ecosystem's convention, so amux composes with the crates around
it without anyone having to reason about compatibility. MIT is simple and
permissive; Apache-2.0 adds an explicit patent grant and a notice requirement.
Offering both lets a recipient take whichever fits what they are building.

It also means the FSL's two-year conversion lands somewhere compatible: an
app version that has become Apache-2.0 can be combined with the core under
Apache-2.0, with no licence mismatch to resolve.

## Contributing

A contribution to the core is dual licensed as above unless you say otherwise,
which is the standard Apache-2.0 inbound term.

A contribution to the app is licensed to us under the FSL's terms, and
converts to Apache-2.0 on the same two-year clock as the version it lands in.

## Marketing and description

The core may be described as open source. The applications may not: the FSL is
not an OSI-approved licence, and calling it open source would be wrong even
though the source is published. "Source-available" is the accurate phrase, and
the two-year conversion is worth stating alongside it, because it is the part
that makes the restriction temporary.

## What this does not cover

Third-party code vendored into the repository keeps its own licence, recorded
beside it. `crates/shot/assets/DejaVu-LICENSE.txt` is one such file.

Trademarks are not licensed by either licence. The FSL says so explicitly; the
same holds for the core.
