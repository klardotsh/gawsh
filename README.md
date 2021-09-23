# gawsh: a static site generator to peruse git repositories

> Gawsh darn, that's a lot of HTML...

`gawsh` is a highly opinionated tool to generate simple, static page fragments
used to browse a Git repository. It's meant to compliment, but not integrate
directly with, existing static site generators, allowing a low-friction visual
transition from, perhaps, a blog to ones' code. By extension, it seamlessly
supports being embedded within a blog or blog post, because _it just generates
static page fragments_.

This tool isn't the first player in this space, though it wasn't far off,
somehow. The most feature-complete alternative I've come across is
[stagit](https://codemadness.org/stagit.html), which is also excellent and
worth your consideration.

This tool makes no attempt to be a social network, to understand the concepts
of "users" or "accounts", to manage or otherwise do anything to a repository
that might require write access, or to deploy anything to anywhere. It also
does not, and will not, introduce analytics, telemetry, or other forms of
tracking to its output. If you're looking for any of the above, consider
another service. Here's a few that are libre software, though not all of these
do all (or even most) of the above:

- [Gitea](https://gitea.com/), or perhaps a hosted version thereof, for example
  [Codeberg](https://codeberg.org/)
- [Gogs](https://gogs.io/)
- [Sourcehut](https://sourcehut.org/)
- [cgit](https://git.zx2c4.com/cgit/)
- [GitLab](https://gitlab.com/)

## Getting Started

_This section reserved, to be filled in eventually..._

`gawsh` has very little configuration of its own, and does not have a config
file. Its few options are all passed on the command line:

- help text goes here

## Where can I use it?

`gawsh` should generally run anywhere a Rust 1.55+ compiler can target, against
any Git repository `libgit2` can understand, and its output is plain UTF-8 text
you can upload to just about anywhere you want. Yep, even that FTP host you
last used in 1999. It doesn't integrate with any CI systems or, for example,
GitHub Pages, out of the box - that's left as an exercise to the reader, or to
some other program, probably by some other author.

On the viewing end, the output files should be legible, even if not perfect, in
anything capable of rendering basic, standards-compliant HTML5 fragments. In
general, this means `gawsh` sites should work to some basic degree (or better)
in browsers like [Lynx](https://invisible-island.net/lynx/) or
[Netsurf](https://www.netsurf-browser.org/).

## Copying, Contributing, and Legal

`gawsh`'s implementation, specification, documentation, artwork, and other
assets are all [Copyfree](http://copyfree.org/), released under the [Creative
Commons Zero 1.0
dedication](https://creativecommons.org/publicdomain/zero/1.0/). This means
you're free to use it for any purpose, in any context, and without letting me
know.

Contributions will be considered, but are not guaranteed to be merged for any
reason or no reason at all. By submitting a contribution to `gawsh`, you assert
the following (this is the [Unlicense waiver](https://unlicense.org/WAIVER)):

> I dedicate any and all copyright interest in this software to the
> public domain. I make this dedication for the benefit of the public at
> large and to the detriment of my heirs and successors. I intend this
> dedication to be an overt act of relinquishment in perpetuity of all
> present and future rights to this software under copyright law.
>
> To the best of my knowledge and belief, my contributions are either
> originally authored by me or are derived from prior works which I have
> verified are also in the public domain and are not subject to claims
> of copyright by other parties.
>
> To the best of my knowledge and belief, no individual, business,
> organization, government, or other entity has any copyright interest
> in my contributions, and I affirm that I will not make contributions
> that are otherwise encumbered.
